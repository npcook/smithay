use crate::{
    output::{Output, WeakOutput},
    utils::{user_data::UserDataMap, SealedFile},
    wayland::compositor::{self, Cacheable, SurfaceData},
};

mod dispatch;

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    hash::{Hash, Hasher},
    io::{Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};
use wayland_protocols::wp::color_management::v1::server::{
    wp_color_management_output_v1::WpColorManagementOutputV1,
    wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
    wp_color_management_surface_v1::WpColorManagementSurfaceV1,
    wp_color_manager_v1::{Feature, Primaries, RenderIntent, TransferFunction, WpColorManagerV1},
    wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1,
    wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
    wp_image_description_v1::WpImageDescriptionV1,
};
use wayland_server::{
    protocol::wl_surface::WlSurface, Dispatch, DisplayHandle, GlobalDispatch, Resource, Weak,
};

static NEXT_IMAGE_DESCRIPTION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Debug)]
pub struct ColorManagementState {
    supported_rendering_intents: HashSet<RenderIntent>,
    supported_features: HashSet<Feature>,
    supported_tf: HashSet<TransferFunction>,
    supported_primaries: HashSet<Primaries>,
    known_image_descriptions: HashMap<ImageDescriptionContents, std::sync::Weak<ImageDescriptionInternal>>,
}

pub trait ColorManagementHandler {
    fn color_management_state(&mut self) -> &mut ColorManagementState;
    fn verify_icc(&mut self, icc_data: &[u8]) -> bool;
    fn description_for_output(&mut self, output: &Output) -> ImageDescription;
    fn preferred_description_for_surface(&mut self, surface: &WlSurface) -> ImageDescription;
}

#[derive(Debug)]
pub struct ColorManagementOutput {
    description: Mutex<ImageDescription>,
    known_instances: Mutex<Vec<WpColorManagementOutputV1>>,
}

impl ColorManagementOutput {
    fn new(desc: ImageDescription) -> Self {
        ColorManagementOutput {
            description: Mutex::new(desc),
            known_instances: Mutex::new(Vec::new()),
        }
    }

    fn add_instance(&self, instance: WpColorManagementOutputV1) {
        self.known_instances.lock().unwrap().push(instance);
    }

    fn remove_instance(&self, instance: &WpColorManagementOutputV1) {
        self.known_instances.lock().unwrap().retain(|i| i != instance);
    }
}

pub fn get_surface_description(surface: &WlSurface) -> (Option<ImageDescription>, RenderIntent) {
    compositor::with_states(surface, get_surface_description_from_surface_data)
}

pub fn get_output_description(output: &Output) -> Option<ImageDescription> {
    output
        .user_data()
        .get::<ColorManagementOutput>()
        .map(|data| data.description.lock().unwrap().clone())
}

pub fn get_surface_description_from_surface_data(
    states: &SurfaceData,
) -> (Option<ImageDescription>, RenderIntent) {
    let data = states
        .cached_state
        .get::<ColorManagementSurfaceCachedState>()
        .current()
        .clone();
    (data.description, data.render_intent)
}

pub fn update_surface_preferred(states: &SurfaceData, preferred: ImageDescription) {
    let preferred_id = preferred.0.id;

    let Some(data) = states.data_map.get::<ColorManagementSurfaceData>() else {
        return;
    };

    {
        let mut current_preferred = data.preferred.lock().unwrap();
        if current_preferred.0.id == preferred.0.id {
            return;
        }
        *current_preferred = preferred;
    }
    for feedback in data.known_feedback_instances.lock().unwrap().iter() {
        if feedback.version() >= 2 {
            feedback.preferred_changed2((preferred_id >> 32) as u32, preferred_id as u32);
        } else {
            feedback.preferred_changed(preferred_id as u32);
        }
    }
}

#[derive(Debug, Clone)]
struct ColorManagementSurfaceCachedState {
    description: Option<ImageDescription>,
    render_intent: RenderIntent,
}

impl Default for ColorManagementSurfaceCachedState {
    fn default() -> Self {
        ColorManagementSurfaceCachedState {
            description: None,
            render_intent: RenderIntent::Perceptual,
        }
    }
}

impl Cacheable for ColorManagementSurfaceCachedState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        self.clone()
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

#[derive(Debug)]
pub struct ColorManagementSurfaceData {
    preferred: Mutex<ImageDescription>,
    known_instances: Mutex<Vec<WpColorManagementSurfaceV1>>,
    known_feedback_instances: Mutex<Vec<WpColorManagementSurfaceFeedbackV1>>,
}

impl ColorManagementSurfaceData {
    fn new(preferred_desc: ImageDescription) -> Self {
        Self {
            preferred: Mutex::new(preferred_desc),
            known_instances: Mutex::new(Vec::new()),
            known_feedback_instances: Mutex::new(Vec::new()),
        }
    }

    fn add_instance(&self, instance: WpColorManagementSurfaceV1) {
        self.known_instances.lock().unwrap().push(instance);
    }

    fn remove_instance(&self, instance: &WpColorManagementSurfaceV1) {
        self.known_instances.lock().unwrap().retain(|i| i != instance);
    }

    fn add_feedback_instance(&self, instance: WpColorManagementSurfaceFeedbackV1) {
        self.known_feedback_instances.lock().unwrap().push(instance);
    }

    fn remove_feedback_instance(&self, instance: &WpColorManagementSurfaceFeedbackV1) {
        self.known_feedback_instances
            .lock()
            .unwrap()
            .retain(|i| i != instance);
    }
}

#[derive(Debug)]
pub struct ImageDescriptionData {
    get_information: bool,
    info: ImageDescription,
}

#[derive(Debug, Clone)]
pub struct ImageDescription(Arc<ImageDescriptionInternal>);

impl PartialEq for ImageDescription {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl ImageDescription {
    pub fn contents(&self) -> &ImageDescriptionContents {
        &self.0.contents
    }
    pub fn user_data(&self) -> &UserDataMap {
        &self.0.user_data
    }
}

#[derive(Debug)]
pub struct ImageDescriptionInternal {
    id: u64,
    contents: ImageDescriptionContents,
    user_data: UserDataMap,
}

#[derive(Debug, Clone)]
pub struct IccData {
    data: Vec<u8>,
    file: Arc<Mutex<Option<SealedFile>>>,
}

impl AsRef<[u8]> for IccData {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub struct Luminance {
    pub min: u32,
    pub max: u32,
    pub reference: u32,
}

#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
pub struct MasteringLuminance {
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone)]
pub enum ImageDescriptionContents {
    ICC(IccData),
    Parametric {
        tf: TransferFunctionEnum,
        primaries: PrimariesEnum,
        luminances: Option<Luminance>,
        target_primaries: Option<ParametricPrimaries>,
        target_luminance: Option<MasteringLuminance>,
        max_cll: Option<u32>,
        max_fall: Option<u32>,
    },
}

impl Hash for ImageDescriptionContents {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            ImageDescriptionContents::ICC(IccData { data, .. }) => {
                data.hash(state);
            }
            ImageDescriptionContents::Parametric {
                tf,
                primaries,
                luminances,
                target_primaries,
                target_luminance,
                max_cll,
                max_fall,
            } => {
                tf.hash(state);
                primaries.hash(state);
                luminances.hash(state);
                target_primaries.hash(state);
                target_luminance.hash(state);
                max_cll.hash(state);
                max_fall.hash(state);
            }
        }
    }
}

impl PartialEq for ImageDescriptionContents {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                ImageDescriptionContents::ICC(IccData { data: data1, .. }),
                ImageDescriptionContents::ICC(IccData { data: data2, .. }),
            ) => data1 == data2,
            (
                ImageDescriptionContents::Parametric {
                    tf: tf1,
                    primaries: primaries1,
                    luminances: luminances1,
                    target_primaries: target_primaries1,
                    target_luminance: target_luminance1,
                    max_cll: max_ccl1,
                    max_fall: max_fall1,
                },
                ImageDescriptionContents::Parametric {
                    tf: tf2,
                    primaries: primaries2,
                    luminances: luminances2,
                    target_primaries: target_primaries2,
                    target_luminance: target_luminance2,
                    max_cll: max_ccl2,
                    max_fall: max_fall2,
                },
            ) => {
                tf1 == tf2
                    && primaries1 == primaries2
                    && luminances1 == luminances2
                    && target_primaries1 == target_primaries2
                    && target_luminance1 == target_luminance2
                    && max_ccl1 == max_ccl2
                    && max_fall1 == max_fall2
            }
            _ => false,
        }
    }
}

impl Eq for ImageDescriptionContents {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferFunctionEnum {
    Named(TransferFunction),
    Power(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimariesEnum {
    Named(Primaries),
    Parametric(ParametricPrimaries),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParametricPrimaries {
    pub red: (i32, i32),
    pub green: (i32, i32),
    pub blue: (i32, i32),
    pub white: (i32, i32),
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum DescriptionError {
    #[error("incomplete parameter set")]
    IncompleteSet,
    #[error("invalid combination of parameters")]
    InconsistentSet,
}

impl ColorManagementState {
    pub fn new<D>(
        dh: &DisplayHandle,
        supported_rendering_intents: impl Iterator<Item = RenderIntent>,
        supported_features: impl Iterator<Item = Feature>,
        supported_tf: impl Iterator<Item = TransferFunction>,
        supported_primaries: impl Iterator<Item = Primaries>,
    ) -> ColorManagementState
    where
        D: ColorManagementHandler
            + GlobalDispatch<WpColorManagerV1, (), D>
            + Dispatch<WpColorManagerV1, (), D>
            + Dispatch<WpColorManagementOutputV1, WeakOutput, D>
            + Dispatch<WpColorManagementSurfaceV1, Weak<WlSurface>, D>
            + Dispatch<WpImageDescriptionV1, (), D>
            + Dispatch<WpImageDescriptionV1, ImageDescriptionData, D>
            + Dispatch<WpImageDescriptionCreatorIccV1, Mutex<Option<ImageDescriptionIccBuilder>>, D>
            + Dispatch<WpImageDescriptionCreatorParamsV1, Mutex<Option<ImageDescriptionParametricBuilder>>, D>
            + 'static,
    {
        dh.create_global::<D, WpColorManagerV1, ()>(1, ());
        ColorManagementState {
            supported_rendering_intents: supported_rendering_intents.collect(),
            supported_features: supported_features.collect(),
            supported_tf: supported_tf.collect(),
            supported_primaries: supported_primaries.collect(),
            known_image_descriptions: HashMap::new(),
        }
    }

    pub fn build_description(&mut self, contents: ImageDescriptionContents) -> ImageDescription {
        struct ImageDescriptionWrapper(ImageDescriptionContents);
        impl TryInto<ImageDescriptionContents> for ImageDescriptionWrapper {
            type Error = DescriptionError;
            fn try_into(self) -> Result<ImageDescriptionContents, Self::Error> {
                Ok(self.0)
            }
        }

        self.build_description_internal(ImageDescriptionWrapper(contents))
            .unwrap()
    }

    fn build_description_internal<B: TryInto<ImageDescriptionContents, Error = DescriptionError>>(
        &mut self,
        contents: B,
    ) -> Result<ImageDescription, DescriptionError> {
        let contents = contents.try_into()?;
        let desc = match self
            .known_image_descriptions
            .get(&contents)
            .and_then(std::sync::Weak::upgrade)
        {
            Some(desc) => desc,
            None => {
                let desc = Arc::new(ImageDescriptionInternal {
                    id: NEXT_IMAGE_DESCRIPTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                    contents: contents.clone(),
                    user_data: UserDataMap::new(),
                });
                self.known_image_descriptions
                    .insert(contents, Arc::downgrade(&desc));
                desc
            }
        };

        Ok(ImageDescription(desc))
    }
}

#[derive(Debug, Default)]
pub struct ImageDescriptionIccBuilder {
    data: Option<Vec<u8>>,
}

impl ImageDescriptionIccBuilder {
    pub fn with_data(&mut self, data: impl AsRef<[u8]>) -> bool {
        let result = self.data.is_some();
        self.data = Some(Vec::from(data.as_ref()));
        result
    }

    pub fn with_file(&mut self, mut file: File, offset: usize, len: usize) -> Result<bool, std::io::Error> {
        let result = self.data.is_some();
        file.seek(SeekFrom::Start(offset as u64))?;

        let mut data = Vec::with_capacity(len);
        let mut buf = [0u8; 4096];

        while let Ok(size) = file.read(&mut buf) {
            if data.len() + size >= len {
                data.extend(&buf[0..(len - data.len())]);
                break;
            } else {
                data.extend(&buf);
            }
        }
        self.data = Some(data);

        Ok(result)
    }
}

impl TryInto<ImageDescriptionContents> for ImageDescriptionIccBuilder {
    type Error = DescriptionError;
    fn try_into(self) -> Result<ImageDescriptionContents, Self::Error> {
        if self.data.is_none() {
            return Err(DescriptionError::IncompleteSet);
        }

        Ok(ImageDescriptionContents::ICC(IccData {
            data: self.data.unwrap(),
            file: Arc::new(Mutex::new(None)),
        }))
    }
}

#[derive(Debug, Default)]
pub struct ImageDescriptionParametricBuilder {
    tf: Option<TransferFunctionEnum>,
    primaries: Option<PrimariesEnum>,
    luminances: Option<Luminance>,
    target_primaries: Option<ParametricPrimaries>,
    target_luminance: Option<MasteringLuminance>,
    max_cll: Option<u32>,
    max_fall: Option<u32>,
}

impl ImageDescriptionParametricBuilder {
    pub fn set_tf(&mut self, tf: TransferFunctionEnum) -> bool {
        let result = self.tf.is_some();
        self.tf = Some(tf);
        result
    }

    pub fn set_primaries(&mut self, primaries: PrimariesEnum) -> bool {
        let result = self.primaries.is_some();
        self.primaries = Some(primaries);
        result
    }

    pub fn set_target_primaries(&mut self, target_primaries: ParametricPrimaries) -> bool {
        let result = self.target_primaries.is_some();
        self.target_primaries = Some(target_primaries);
        result
    }

    pub fn set_target_luminance(&mut self, target_luminance: MasteringLuminance) -> bool {
        let result = self.target_luminance.is_some();
        self.target_luminance = Some(target_luminance);
        result
    }

    pub fn set_luminances(&mut self, luminances: Luminance) -> bool {
        let result = self.luminances.is_some();
        self.luminances = Some(luminances);
        result
    }

    pub fn set_max_cll(&mut self, max_ccl: u32) -> bool {
        let result = self.max_cll.is_some();
        self.max_cll = Some(max_ccl);
        result
    }

    pub fn set_max_fall(&mut self, max_fall: u32) -> bool {
        let result = self.max_fall.is_some();
        self.max_fall = Some(max_fall);
        result
    }
}

impl TryInto<ImageDescriptionContents> for ImageDescriptionParametricBuilder {
    type Error = DescriptionError;
    fn try_into(self) -> Result<ImageDescriptionContents, Self::Error> {
        if self.tf.is_none() || self.primaries.is_none() {
            return Err(DescriptionError::IncompleteSet);
        }

        Ok(ImageDescriptionContents::Parametric {
            tf: self.tf.unwrap(),
            primaries: self.primaries.unwrap(),
            luminances: self.luminances,
            target_primaries: self.target_primaries,
            target_luminance: self.target_luminance,
            max_cll: self.max_cll,
            max_fall: self.max_fall,
        })
    }
}

/// Macro to delegate implementation of the wp color representation protocol
#[macro_export]
macro_rules! delegate_color_management {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        const _: () = {
            use $crate::{
                output::WeakOutput,
                reexports::{
                    wayland_protocols::wp::color_management::v1::server::{
                        wp_color_management_output_v1::WpColorManagementOutputV1,
                        wp_color_management_surface_v1::WpColorManagementSurfaceV1,
                        wp_color_manager_v1::WpColorManagerV1,
                        wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
                        wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1,
                        wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
                        wp_image_description_v1::WpImageDescriptionV1,
                        wp_image_description_info_v1::WpImageDescriptionInfoV1
                    },
                    wayland_server::{delegate_dispatch, delegate_global_dispatch, Weak},
                },
                wayland::color::management::{ ColorManagementState, ImageDescriptionData, ImageDescriptionIccBuilder, ImageDescriptionParametricBuilder }
            };
            use std::sync::Mutex;

            delegate_global_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpColorManagerV1: ()] => ColorManagementState
            );

            delegate_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpColorManagerV1: ()] => ColorManagementState
            );

            delegate_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpColorManagementOutputV1: WeakOutput] => ColorManagementState
            );

            delegate_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpColorManagementSurfaceV1: Weak<WlSurface>] => ColorManagementState
            );

            delegate_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpColorManagementSurfaceFeedbackV1: Weak<WlSurface>] => ColorManagementState
            );

            delegate_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpImageDescriptionV1: ()] => ColorManagementState
            );

            delegate_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpImageDescriptionV1: ImageDescriptionData] => ColorManagementState
            );

            delegate_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpImageDescriptionInfoV1: ()] => ColorManagementState
            );

            delegate_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpImageDescriptionCreatorIccV1: Mutex<Option<ImageDescriptionIccBuilder>>] => ColorManagementState
            );

            delegate_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpImageDescriptionCreatorParamsV1: Mutex<Option<ImageDescriptionParametricBuilder>>] => ColorManagementState
            );
        };
    };
}
