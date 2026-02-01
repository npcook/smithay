use std::{ffi::CString, os::unix::prelude::AsFd, sync::Mutex};

use crate::{
    output::{Output, WeakOutput},
    utils::SealedFile,
    wayland::{
        color::management::{Luminance, MasteringLuminance},
        compositor,
    },
};

use super::{
    ColorManagementHandler, ColorManagementOutput, ColorManagementState, ColorManagementSurfaceCachedState,
    ColorManagementSurfaceData, DescriptionError, IccData, ImageDescriptionData, ImageDescriptionIccBuilder,
    ImageDescriptionParametricBuilder, ParametricPrimaries, PrimariesEnum, TransferFunctionEnum,
};
use tracing::warn;
use wayland_protocols::wp::color_management::v1::server::{
    wp_color_management_output_v1::{self, WpColorManagementOutputV1},
    wp_color_management_surface_feedback_v1::{self, WpColorManagementSurfaceFeedbackV1},
    wp_color_management_surface_v1::{self, WpColorManagementSurfaceV1},
    wp_color_manager_v1::{self, Feature, WpColorManagerV1},
    wp_image_description_creator_icc_v1::{self, WpImageDescriptionCreatorIccV1},
    wp_image_description_creator_params_v1::{self, WpImageDescriptionCreatorParamsV1},
    wp_image_description_info_v1::{self, WpImageDescriptionInfoV1},
    wp_image_description_v1::{self, WpImageDescriptionV1},
};
use wayland_server::{
    protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
    Resource, WEnum, Weak,
};

impl<D> GlobalDispatch<WpColorManagerV1, (), D> for ColorManagementState
where
    D: ColorManagementHandler
        + GlobalDispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagementOutputV1, WeakOutput, D>
        + Dispatch<WpColorManagementSurfaceV1, Weak<WlSurface>, D>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, Weak<WlSurface>, D>
        + Dispatch<WpImageDescriptionV1, (), D>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData, D>
        + Dispatch<WpImageDescriptionCreatorIccV1, Mutex<Option<ImageDescriptionIccBuilder>>, D>
        + Dispatch<WpImageDescriptionCreatorParamsV1, Mutex<Option<ImageDescriptionParametricBuilder>>, D>
        + Dispatch<WpImageDescriptionInfoV1, (), D>
        + 'static,
{
    fn bind(
        state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WpColorManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        let state = state.color_management_state();
        let instance = data_init.init(resource, ());

        for feature in &state.supported_features {
            instance.supported_feature(*feature);
        }
        for intent in &state.supported_rendering_intents {
            instance.supported_intent(*intent);
        }
        for code_point in &state.supported_tf {
            instance.supported_tf_named(*code_point);
        }
        for code_point in &state.supported_primaries {
            instance.supported_primaries_named(*code_point);
        }
        instance.done();
    }
}

impl<D> Dispatch<WpColorManagerV1, (), D> for ColorManagementState
where
    D: ColorManagementHandler
        + GlobalDispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagementOutputV1, WeakOutput, D>
        + Dispatch<WpColorManagementSurfaceV1, Weak<WlSurface>, D>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, Weak<WlSurface>, D>
        + Dispatch<WpImageDescriptionV1, (), D>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData, D>
        + Dispatch<WpImageDescriptionCreatorIccV1, Mutex<Option<ImageDescriptionIccBuilder>>, D>
        + Dispatch<WpImageDescriptionCreatorParamsV1, Mutex<Option<ImageDescriptionParametricBuilder>>, D>
        + Dispatch<WpImageDescriptionInfoV1, (), D>
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &WpColorManagerV1,
        request: wp_color_manager_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        use wp_color_manager_v1::{Error, Request};
        // println!("{:?}", request);
        match request {
            Request::GetOutput { id, output } => {
                let Some(output) = Output::from_resource(&output) else {
                    resource.post_error(Error::UnsupportedFeature, "WlOutput has no associated `Output`");
                    return;
                };

                let color_output = output
                    .user_data()
                    .get_or_insert(|| ColorManagementOutput::new(state.description_for_output(&output)));

                let instance = data_init.init(id, output.downgrade());
                color_output.add_instance(instance);
            }
            Request::GetSurface { id, surface } => {
                compositor::with_states(&surface, |states| {
                    let data = states.data_map.get_or_insert_threadsafe(|| {
                        ColorManagementSurfaceData::new(state.preferred_description_for_surface(&surface))
                    });

                    let instance = data_init.init(id, surface.downgrade());
                    data.add_instance(instance);
                });
            }
            Request::GetSurfaceFeedback { id, surface } => {
                compositor::with_states(&surface, |states| {
                    let data = states.data_map.get_or_insert_threadsafe(|| {
                        ColorManagementSurfaceData::new(state.preferred_description_for_surface(&surface))
                    });

                    let preferred_id = data.preferred.lock().unwrap().0.id;
                    let instance = data_init.init(id, surface.downgrade());
                    if instance.version() >= 2 {
                        instance.preferred_changed2((preferred_id >> 32) as u32, preferred_id as u32);
                    } else {
                        instance.preferred_changed(preferred_id as u32);
                    }
                    data.add_feedback_instance(instance);
                });
            }
            Request::CreateIccCreator { obj } => {
                let state = state.color_management_state();
                if !state.supported_features.contains(&Feature::IccV2V4) {
                    resource.post_error(
                        Error::UnsupportedFeature,
                        "Compositor doesn't support the ICC image description creator",
                    );
                    return;
                }

                data_init.init(obj, Mutex::new(Some(ImageDescriptionIccBuilder::default())));
            }
            Request::CreateParametricCreator { obj } => {
                let state = state.color_management_state();
                if !state.supported_features.contains(&Feature::Parametric) {
                    resource.post_error(
                        Error::UnsupportedFeature,
                        "Compositor doesn't support the Parametric image description creator",
                    );
                    return;
                }

                data_init.init(
                    obj,
                    Mutex::new(Some(ImageDescriptionParametricBuilder::default())),
                );
            }
            _ => {}
        }
    }
}

impl<D> Dispatch<WpColorManagementOutputV1, WeakOutput, D> for ColorManagementState
where
    D: ColorManagementHandler
        + GlobalDispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagementOutputV1, WeakOutput, D>
        + Dispatch<WpColorManagementSurfaceV1, Weak<WlSurface>, D>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, Weak<WlSurface>, D>
        + Dispatch<WpImageDescriptionV1, (), D>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData, D>
        + Dispatch<WpImageDescriptionCreatorIccV1, Mutex<Option<ImageDescriptionIccBuilder>>, D>
        + Dispatch<WpImageDescriptionCreatorParamsV1, Mutex<Option<ImageDescriptionParametricBuilder>>, D>
        + Dispatch<WpImageDescriptionInfoV1, (), D>
        + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &WpColorManagementOutputV1,
        request: wp_color_management_output_v1::Request,
        data: &WeakOutput,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        // println!("{:?}", request);
        match request {
            wp_color_management_output_v1::Request::GetImageDescription { image_description } => {
                if let Some(output) = data.upgrade() {
                    let data = output.user_data().get::<ColorManagementOutput>().unwrap();
                    let info = data.description.lock().unwrap().clone();
                    let instance = data_init.init(
                        image_description,
                        ImageDescriptionData {
                            get_information: true,
                            info: info.clone(),
                        },
                    );
                    image_description_ready(&instance, info.0.id);
                } else {
                    let failed_desc = data_init.init(image_description, ());
                    failed_desc.failed(
                        wp_image_description_v1::Cause::NoOutput,
                        "Output was destroyed".into(),
                    );
                }
            }
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: wayland_backend::server::ClientId,
        resource: &WpColorManagementOutputV1,
        data: &WeakOutput,
    ) {
        if let Some(output) = data.upgrade() {
            if let Some(data) = output.user_data().get::<ColorManagementOutput>() {
                data.remove_instance(resource);
            }
        }
    }
}

impl<D> Dispatch<WpColorManagementSurfaceV1, Weak<WlSurface>, D> for ColorManagementState
where
    D: ColorManagementHandler
        + GlobalDispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagementOutputV1, WeakOutput, D>
        + Dispatch<WpColorManagementSurfaceV1, Weak<WlSurface>, D>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, Weak<WlSurface>, D>
        + Dispatch<WpImageDescriptionV1, (), D>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData, D>
        + Dispatch<WpImageDescriptionCreatorIccV1, Mutex<Option<ImageDescriptionIccBuilder>>, D>
        + Dispatch<WpImageDescriptionCreatorParamsV1, Mutex<Option<ImageDescriptionParametricBuilder>>, D>
        + Dispatch<WpImageDescriptionInfoV1, (), D>
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &WpColorManagementSurfaceV1,
        request: wp_color_management_surface_v1::Request,
        data: &Weak<WlSurface>,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        // println!("{:?}", request);
        use wp_color_management_surface_v1::{Error, Request};
        match request {
            Request::SetImageDescription {
                image_description,
                render_intent,
            } => {
                if let Ok(surface) = data.upgrade() {
                    if let Some(data) = image_description.data::<ImageDescriptionData>() {
                        compositor::with_states(&surface, |states| {
                            *states.cached_state.get().pending() = ColorManagementSurfaceCachedState {
                                description: Some(data.info.clone()),
                                render_intent: match render_intent {
                                    WEnum::Value(val) => {
                                        let state = state.color_management_state();
                                        if state.supported_rendering_intents.contains(&val) {
                                            val
                                        } else {
                                            resource
                                                .post_error(Error::RenderIntent, "Unsupported render intent");
                                            return;
                                        }
                                    }
                                    WEnum::Unknown(_) => {
                                        resource.post_error(
                                            Error::RenderIntent,
                                            "Unknown render intent (wrong version?)",
                                        );
                                        return;
                                    }
                                },
                            };
                            // println!(
                            //     "Successfully set surface description on surface {}: {:?}",
                            //     surface.id(),
                            //     data.info
                            // );
                        })
                    } else {
                        image_description.post_error(
                            Error::Inert,
                            "Tried to set a failed image description on a surface",
                        );
                        return;
                    }
                }
            }
            Request::UnsetImageDescription => {
                if let Ok(surface) = data.upgrade() {
                    compositor::with_states(&surface, |states| {
                        *states.cached_state.get().pending() = ColorManagementSurfaceCachedState {
                            description: None,
                            ..Default::default()
                        };
                    });
                } else {
                    resource.post_error(Error::Inert, "Surface doesn't exist");
                    return;
                }
            }
            Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: wayland_backend::server::ClientId,
        resource: &WpColorManagementSurfaceV1,
        data: &Weak<WlSurface>,
    ) {
        if let Ok(surface) = data.upgrade() {
            compositor::with_states(&surface, |states| {
                if let Some(data) = states.data_map.get::<ColorManagementSurfaceData>() {
                    data.remove_instance(resource);
                }
            })
        }
    }
}

impl<D> Dispatch<WpColorManagementSurfaceFeedbackV1, Weak<WlSurface>, D> for ColorManagementState
where
    D: ColorManagementHandler
        + GlobalDispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagementOutputV1, WeakOutput, D>
        + Dispatch<WpColorManagementSurfaceV1, Weak<WlSurface>, D>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, Weak<WlSurface>, D>
        + Dispatch<WpImageDescriptionV1, (), D>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData, D>
        + Dispatch<WpImageDescriptionCreatorIccV1, Mutex<Option<ImageDescriptionIccBuilder>>, D>
        + Dispatch<WpImageDescriptionCreatorParamsV1, Mutex<Option<ImageDescriptionParametricBuilder>>, D>
        + Dispatch<WpImageDescriptionInfoV1, (), D>
        + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &WpColorManagementSurfaceFeedbackV1,
        request: wp_color_management_surface_feedback_v1::Request,
        data: &Weak<WlSurface>,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        // println!("{:?}", request);
        use wp_color_management_surface_feedback_v1::Request;
        match request {
            Request::GetPreferred { image_description } => {
                if let Ok(surface) = data.upgrade() {
                    compositor::with_states(&surface, |states| {
                        let data = states.data_map.get::<ColorManagementSurfaceData>().unwrap();
                        let info = data.preferred.lock().unwrap().clone();
                        let instance = data_init.init(
                            image_description,
                            ImageDescriptionData {
                                get_information: true,
                                info: info.clone(),
                            },
                        );
                        image_description_ready(&instance, info.0.id);
                    });
                } else {
                    let failed_desc = data_init.init(image_description, ());
                    failed_desc.failed(
                        wp_image_description_v1::Cause::NoOutput,
                        "Surface was destroyed".into(),
                    );
                }
            }
            Request::GetPreferredParametric { image_description } => {
                if let Ok(surface) = data.upgrade() {
                    compositor::with_states(&surface, |states| {
                        let data = states.data_map.get::<ColorManagementSurfaceData>().unwrap();
                        let info = data.preferred.lock().unwrap().clone();
                        let instance = data_init.init(
                            image_description,
                            ImageDescriptionData {
                                get_information: true,
                                info: info.clone(),
                            },
                        );
                        image_description_ready(&instance, info.0.id);
                        todo!();
                    });
                } else {
                    let failed_desc = data_init.init(image_description, ());
                    failed_desc.failed(
                        wp_image_description_v1::Cause::NoOutput,
                        "Surface was destroyed".into(),
                    );
                }
            }
            Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: wayland_backend::server::ClientId,
        resource: &WpColorManagementSurfaceFeedbackV1,
        data: &Weak<WlSurface>,
    ) {
        if let Ok(surface) = data.upgrade() {
            compositor::with_states(&surface, |states| {
                if let Some(data) = states.data_map.get::<ColorManagementSurfaceData>() {
                    data.remove_feedback_instance(resource);
                }
            })
        }
    }
}

impl<D> Dispatch<WpImageDescriptionV1, (), D> for ColorManagementState
where
    D: ColorManagementHandler
        + GlobalDispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagementOutputV1, WeakOutput, D>
        + Dispatch<WpColorManagementSurfaceV1, Weak<WlSurface>, D>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, Weak<WlSurface>, D>
        + Dispatch<WpImageDescriptionV1, (), D>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData, D>
        + Dispatch<WpImageDescriptionCreatorIccV1, Mutex<Option<ImageDescriptionIccBuilder>>, D>
        + Dispatch<WpImageDescriptionCreatorParamsV1, Mutex<Option<ImageDescriptionParametricBuilder>>, D>
        + Dispatch<WpImageDescriptionInfoV1, (), D>
        + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        resource: &WpImageDescriptionV1,
        request: wp_image_description_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        // println!("{:?}", request);
        use wp_image_description_v1::{Error, Request};
        match request {
            Request::Destroy => {}
            _ => resource.post_error(Error::NotReady, "Image description had failed"),
        }
    }
}

impl<D> Dispatch<WpImageDescriptionV1, ImageDescriptionData, D> for ColorManagementState
where
    D: ColorManagementHandler
        + GlobalDispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagerV1, (), D>
        + Dispatch<WpColorManagementOutputV1, WeakOutput, D>
        + Dispatch<WpColorManagementSurfaceV1, Weak<WlSurface>, D>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, Weak<WlSurface>, D>
        + Dispatch<WpImageDescriptionV1, (), D>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData, D>
        + Dispatch<WpImageDescriptionCreatorIccV1, Mutex<Option<ImageDescriptionIccBuilder>>, D>
        + Dispatch<WpImageDescriptionCreatorParamsV1, Mutex<Option<ImageDescriptionParametricBuilder>>, D>
        + Dispatch<WpImageDescriptionInfoV1, (), D>
        + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        resource: &WpImageDescriptionV1,
        request: wp_image_description_v1::Request,
        data: &ImageDescriptionData,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        // println!("{:?}", request);
        use wp_image_description_v1::{Cause, Error, Request};
        match request {
            Request::GetInformation { information } => {
                if !data.get_information {
                    resource.post_error(Error::NoInformation, "Constructor doesn't allow get_information");
                    return;
                }

                let instance = data_init.init(information, ());

                match &data.info.0.contents {
                    super::ImageDescriptionContents::ICC(IccData { data, file }) => {
                        let mut file = file.lock().unwrap();
                        if file.is_none() {
                            match SealedFile::with_data(&CString::new("icc").unwrap(), data) {
                                Ok(new_file) => {
                                    *file = Some(new_file);
                                }
                                Err(err) => {
                                    warn!(?err, "File to create memory map for icc file");
                                    resource.failed(Cause::Unsupported, "Internal error".into());
                                    return;
                                }
                            };
                        }
                        if let Some(file) = file.as_ref() {
                            instance.icc_file(file.as_fd(), file.size() as u32);
                        }
                    }
                    super::ImageDescriptionContents::Parametric {
                        tf,
                        primaries,
                        luminances,
                        target_primaries,
                        target_luminance,
                        max_cll: max_ccl,
                        max_fall,
                    } => {
                        match tf {
                            TransferFunctionEnum::Named(name) => instance.tf_named(*name),
                            TransferFunctionEnum::Power(pow) => instance.tf_power(*pow),
                        };
                        match primaries {
                            PrimariesEnum::Named(name) => instance.primaries_named(*name),
                            PrimariesEnum::Parametric(ParametricPrimaries {
                                red,
                                green,
                                blue,
                                white,
                            }) => instance
                                .primaries(red.0, red.1, green.0, green.1, blue.0, blue.1, white.0, white.1),
                        };
                        if let Some(luminances) = luminances {
                            instance.luminances(luminances.min, luminances.max, luminances.reference);
                        }
                        if let Some(ParametricPrimaries {
                            red,
                            green,
                            blue,
                            white,
                        }) = target_primaries
                        {
                            instance.target_primaries(
                                red.0, red.1, green.0, green.1, blue.0, blue.1, white.0, white.1,
                            );
                        }
                        if let Some(target_luminance) = target_luminance {
                            instance.target_luminance(target_luminance.min, target_luminance.max);
                        }
                        if let Some(max_ccl) = max_ccl {
                            instance.target_max_cll(*max_ccl);
                        }
                        if let Some(max_fall) = max_fall {
                            instance.target_max_fall(*max_fall);
                        }
                    }
                }

                instance.done();
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut D,
        _client: wayland_backend::server::ClientId,
        _resource: &WpImageDescriptionV1,
        data: &ImageDescriptionData,
    ) {
        // println!("destroyed");
        state
            .color_management_state()
            .known_image_descriptions
            .retain(|_, v| {
                if let Some(v) = v.upgrade() {
                    !std::sync::Arc::ptr_eq(&v, &data.info.0)
                } else {
                    false
                }
            })
    }
}
impl<D> Dispatch<WpImageDescriptionInfoV1, (), D> for ColorManagementState
where
    D: Dispatch<WpImageDescriptionInfoV1, (), D> + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &WpImageDescriptionInfoV1,
        _request: wp_image_description_info_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
    }

    fn destroyed(
        _state: &mut D,
        _client: wayland_backend::server::ClientId,
        _resource: &WpImageDescriptionInfoV1,
        _data: &(),
    ) {
    }
}

impl<D> Dispatch<WpImageDescriptionCreatorIccV1, Mutex<Option<ImageDescriptionIccBuilder>>, D>
    for ColorManagementState
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
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &WpImageDescriptionCreatorIccV1,
        request: wp_image_description_creator_icc_v1::Request,
        data: &Mutex<Option<ImageDescriptionIccBuilder>>,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        // println!("{:?}", request);
        use wp_image_description_creator_icc_v1::{Error, Request};
        match request {
            Request::SetIccFile {
                icc_profile,
                offset,
                length,
            } => {
                let mut data_guard = data.lock().unwrap();
                let Some(data) = data_guard.as_mut() else {
                    resource.post_error(Error::AlreadySet, "Creator was already used");
                    return;
                };

                if length > 1024 * 1024 * 4 {
                    resource.post_error(Error::BadSize, "Size larger than 4MiB");
                    return;
                }

                let file = std::fs::File::from(icc_profile);
                match data.with_file(file, offset as usize, length as usize) {
                    Ok(true) => {
                        resource.post_error(Error::AlreadySet, "ICC file was already set");
                        return;
                    }
                    Err(err) => {
                        resource.post_error(Error::BadFd, format!("Failed to read ICC file: {}", err));
                        return;
                    }
                    Ok(false) => {}
                }
            }
            Request::Create { image_description } => {
                let mut data_guard = data.lock().unwrap();
                let Some(data) = data_guard.take() else {
                    resource.post_error(Error::AlreadySet, "Creator was already used");
                    return;
                };

                let color_state = state.color_management_state();
                match color_state.build_description_internal(data) {
                    Ok(desc) => {
                        if state.verify_icc(match &desc.0.contents {
                            super::ImageDescriptionContents::ICC(IccData { data, .. }) => data,
                            _ => unreachable!(),
                        }) {
                            let instance = data_init.init(
                                image_description,
                                ImageDescriptionData {
                                    get_information: false,
                                    info: desc.clone(),
                                },
                            );
                            image_description_ready(&instance, desc.0.id);
                        } else {
                            let instance = data_init.init(image_description, ());
                            instance.failed(
                                wp_image_description_v1::Cause::Unsupported,
                                "ICC file failed to parse".into(),
                            );
                            return;
                        }
                    }
                    Err(DescriptionError::IncompleteSet) => {
                        resource.post_error(Error::IncompleteSet, "incomplete parameter set");
                        return;
                    }
                    Err(DescriptionError::InconsistentSet) => {
                        resource.post_error(Error::IncompleteSet, "invalid combination of parameters");
                        return;
                    }
                }
            }
            _ => {}
        }
    }
}

impl<D> Dispatch<WpImageDescriptionCreatorParamsV1, Mutex<Option<ImageDescriptionParametricBuilder>>, D>
    for ColorManagementState
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
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &WpImageDescriptionCreatorParamsV1,
        request: wp_image_description_creator_params_v1::Request,
        data: &Mutex<Option<ImageDescriptionParametricBuilder>>,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        // println!("{:?}", request);
        use wp_image_description_creator_params_v1::{Error, Request};
        let color_state = state.color_management_state();
        match request {
            Request::SetTfNamed { tf } => {
                let mut data_guard = data.lock().unwrap();
                let Some(data) = data_guard.as_mut() else {
                    resource.post_error(Error::AlreadySet, "Creator was already used");
                    return;
                };

                let tf = match tf {
                    WEnum::Value(val) => val,
                    WEnum::Unknown(_) => {
                        resource.post_error(Error::InvalidTf, "Unknown transfer function");
                        return;
                    }
                };
                if !color_state.supported_tf.contains(&tf) {
                    resource.post_error(Error::InvalidTf, "Unsupported transfer function code point");
                    return;
                }

                if data.set_tf(TransferFunctionEnum::Named(tf)) {
                    resource.post_error(Error::AlreadySet, "Transfer function was already set");
                    return;
                }
            }
            Request::SetTfPower { eexp } => {
                let mut data_guard = data.lock().unwrap();
                let Some(data) = data_guard.as_mut() else {
                    resource.post_error(Error::AlreadySet, "Creator was already used");
                    return;
                };

                if !color_state.supported_features.contains(&Feature::SetTfPower) {
                    resource.post_error(Error::UnsupportedFeature, "Unsupported feature set_tf_power");
                    return;
                }

                if eexp < 10000 || eexp > 100000 {
                    resource.post_error(Error::InvalidTf, "Transfer function exponent out of range");
                    return;
                }

                if data.set_tf(TransferFunctionEnum::Power(eexp)) {
                    resource.post_error(Error::AlreadySet, "Transfer function was already set");
                    return;
                }
            }
            Request::SetPrimariesNamed { primaries } => {
                let mut data_guard = data.lock().unwrap();
                let Some(data) = data_guard.as_mut() else {
                    resource.post_error(Error::AlreadySet, "Creator was already used");
                    return;
                };

                let primaries = match primaries {
                    WEnum::Value(val) => val,
                    WEnum::Unknown(_) => {
                        resource.post_error(Error::InvalidTf, "Unknown primaries");
                        return;
                    }
                };
                if !color_state.supported_primaries.contains(&primaries) {
                    resource.post_error(Error::InvalidTf, "Unsupported primaries");
                    return;
                }

                if data.set_primaries(PrimariesEnum::Named(primaries)) {
                    resource.post_error(Error::AlreadySet, "Primaries were already set");
                    return;
                }
            }
            Request::SetPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                let mut data_guard = data.lock().unwrap();
                let Some(data) = data_guard.as_mut() else {
                    // println!("fail1");
                    resource.post_error(Error::AlreadySet, "Creator was already used");
                    return;
                };

                if !color_state.supported_features.contains(&Feature::SetPrimaries) {
                    // println!("fail2");
                    resource.post_error(Error::UnsupportedFeature, "Unsupported feature set_primaries");
                    return;
                }

                if data.set_primaries(PrimariesEnum::Parametric(ParametricPrimaries {
                    red: (r_x, r_y),
                    green: (g_x, g_y),
                    blue: (b_x, b_y),
                    white: (w_x, w_y),
                })) {
                    // println!("fail3");
                    resource.post_error(Error::AlreadySet, "Primaries were already set");
                    return;
                }
                // println!("made it");
            }
            Request::SetMasteringDisplayPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                let mut data_guard = data.lock().unwrap();
                let Some(data) = data_guard.as_mut() else {
                    resource.post_error(Error::AlreadySet, "Creator was already used");
                    return;
                };

                if !color_state
                    .supported_features
                    .contains(&Feature::SetMasteringDisplayPrimaries)
                {
                    resource.post_error(
                        Error::UnsupportedFeature,
                        "Unsupported feature set_mastering_display_primaries",
                    );
                    return;
                }

                if data.set_target_primaries(ParametricPrimaries {
                    red: (r_x, r_y),
                    green: (g_x, g_y),
                    blue: (b_x, b_y),
                    white: (w_x, w_y),
                }) {
                    resource.post_error(Error::AlreadySet, "Mastering Primaries were already set");
                    return;
                }
            }
            Request::SetMasteringLuminance { min_lum, max_lum } => {
                let mut data_guard = data.lock().unwrap();
                let Some(data) = data_guard.as_mut() else {
                    resource.post_error(Error::AlreadySet, "Creator was already used");
                    return;
                };

                if !color_state
                    .supported_features
                    .contains(&Feature::SetMasteringDisplayPrimaries)
                {
                    resource.post_error(
                        Error::UnsupportedFeature,
                        "Unsupported feature set_mastering_luminance",
                    );
                    return;
                }

                if max_lum * 10000 <= min_lum {
                    resource.post_error(Error::InvalidLuminance, "MaxLUM <= MinLUM");
                    return;
                }

                if data.set_target_luminance(MasteringLuminance {
                    min: min_lum,
                    max: max_lum,
                }) {
                    resource.post_error(Error::AlreadySet, "Mastering luminances were already set");
                    return;
                }
            }
            Request::SetMaxCll { max_cll } => {
                let mut data_guard = data.lock().unwrap();
                let Some(data) = data_guard.as_mut() else {
                    resource.post_error(Error::AlreadySet, "Creator was already used");
                    return;
                };

                if data.set_max_cll(max_cll) {
                    resource.post_error(Error::AlreadySet, "Max CCL was already set");
                    return;
                }
            }
            Request::SetMaxFall { max_fall } => {
                let mut data_guard = data.lock().unwrap();
                let Some(data) = data_guard.as_mut() else {
                    resource.post_error(Error::AlreadySet, "Creator was already used");
                    return;
                };

                if data.set_max_fall(max_fall) {
                    resource.post_error(Error::AlreadySet, "Max CCL was already set");
                    return;
                }
            }
            Request::SetLuminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                let mut data_guard = data.lock().unwrap();
                let Some(data) = data_guard.as_mut() else {
                    resource.post_error(Error::AlreadySet, "Creator was already used");
                    return;
                };

                if !color_state.supported_features.contains(&Feature::SetLuminances) {
                    resource.post_error(Error::UnsupportedFeature, "Unsupported feature set_luminances");
                    return;
                }

                if max_lum * 10000 <= min_lum {
                    resource.post_error(Error::InvalidLuminance, "MaxLUM <= MinLUM");
                    return;
                }

                if reference_lum * 10000 <= min_lum {
                    resource.post_error(Error::InvalidLuminance, "RefLUM <= MinLUM");
                    return;
                }

                if data.set_luminances(Luminance {
                    min: min_lum,
                    max: max_lum,
                    reference: reference_lum,
                }) {
                    resource.post_error(Error::AlreadySet, "Luminances were already set");
                    return;
                }
            }
            Request::Create { image_description } => {
                let mut data_guard = data.lock().unwrap();
                let Some(data) = data_guard.take() else {
                    resource.post_error(Error::AlreadySet, "Creator was already used");
                    return;
                };

                match color_state.build_description_internal(data) {
                    Ok(desc) => {
                        let instance = data_init.init(
                            image_description,
                            ImageDescriptionData {
                                get_information: false,
                                info: desc.clone(),
                            },
                        );
                        image_description_ready(&instance, desc.0.id);
                    }
                    Err(DescriptionError::IncompleteSet) => {
                        resource.post_error(Error::IncompleteSet, "incomplete parameter set");
                        return;
                    }
                    Err(DescriptionError::InconsistentSet) => {
                        resource.post_error(Error::IncompleteSet, "invalid combination of parameters");
                        return;
                    }
                }
            }
            _ => {}
        }
    }
}

fn image_description_ready(instance: &WpImageDescriptionV1, id: u64) {
    // println!("Image description ready: {}, v{}", id, instance.version());
    if instance.version() >= 2 {
        instance.ready2((id >> 32) as u32, id as u32);
    } else {
        instance.ready(id as u32);
    }
}
