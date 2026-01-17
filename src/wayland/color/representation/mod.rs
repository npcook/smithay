use wayland_server::{protocol::wl_surface::WlSurface, Dispatch, DisplayHandle, GlobalDispatch, Weak};

use crate::wayland::compositor::{self, Cacheable};

use wayland_protocols::wp::color_representation::v1::server::{
    wp_color_representation_manager_v1::WpColorRepresentationManagerV1,
    wp_color_representation_surface_v1::{
        AlphaMode, ChromaLocation, Coefficients, Range, WpColorRepresentationSurfaceV1,
    },
};
mod dispatch;

use std::sync::Mutex;

#[derive(Debug)]
pub struct ColorRepresentationState {
    coefficients_and_ranges: Vec<(Coefficients, Range)>,
    chroma_locations: Vec<ChromaLocation>,
    alpha_modes: Vec<AlphaMode>,
    known_instances: Vec<WpColorRepresentationManagerV1>,
}

pub trait ColorRepresentationHandler {
    fn color_representation_state(&mut self) -> &mut ColorRepresentationState;
}

#[derive(Debug, Clone, Copy, Default)]
struct ColorRepresentationSurfaceCachedState {
    coefficients_and_range: Option<(Coefficients, Range)>,
    chroma_location: Option<ChromaLocation>,
    alpha_mode: Option<AlphaMode>,
}

impl Cacheable for ColorRepresentationSurfaceCachedState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        *self
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        into.coefficients_and_range = self
            .coefficients_and_range
            .or_else(|| into.coefficients_and_range.take());
        into.chroma_location = self.chroma_location.or_else(|| into.chroma_location.take());
        into.alpha_mode = self.alpha_mode.or_else(|| into.alpha_mode.take());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorRepresentation {
    pub alpha_mode: AlphaMode,
    pub coefficients_and_range: Option<(Coefficients, Range)>,
    pub chroma_location: Option<ChromaLocation>,
}

pub fn get_color_representation(surface: &WlSurface) -> ColorRepresentation {
    compositor::with_states(surface, |states| {
        let cached = states
            .cached_state
            .get::<ColorRepresentationSurfaceCachedState>()
            .pending()
            .clone();

        ColorRepresentation {
            alpha_mode: cached.alpha_mode.unwrap_or(AlphaMode::PremultipliedElectrical),
            coefficients_and_range: cached.coefficients_and_range,
            chroma_location: cached.chroma_location,
        }
    })
}

#[derive(Debug)]
struct ColorRepresentationSurfaceData {
    instance: Mutex<Option<WpColorRepresentationSurfaceV1>>,
}

impl ColorRepresentationSurfaceData {
    fn new() -> Self {
        Self {
            instance: Mutex::new(None),
        }
    }

    fn is_resource_attached(&self) -> bool {
        self.instance.lock().unwrap().is_some()
    }
}

impl ColorRepresentationState {
    pub fn new<D>(
        dh: &DisplayHandle,
        coefficients_and_ranges: impl Iterator<Item = (Coefficients, Range)>,
        chroma_locations: impl Iterator<Item = ChromaLocation>,
        alpha_modes: impl Iterator<Item = AlphaMode>,
    ) -> ColorRepresentationState
    where
        D: GlobalDispatch<WpColorRepresentationManagerV1, ()>
            + Dispatch<WpColorRepresentationManagerV1, ()>
            + Dispatch<WpColorRepresentationSurfaceV1, Weak<WlSurface>>
            + ColorRepresentationHandler
            + 'static,
    {
        dh.create_global::<D, WpColorRepresentationManagerV1, ()>(1, ());
        ColorRepresentationState {
            coefficients_and_ranges: coefficients_and_ranges.collect(),
            chroma_locations: chroma_locations.collect(),
            alpha_modes: alpha_modes.collect(),
            known_instances: Vec::new(),
        }
    }
}

/// Macro to delegate implementation of the wp color representation protocol
#[macro_export]
macro_rules! delegate_color_representation {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        const _: () = {
            use $crate::{
                reexports::{
                    wayland_protocols::wp::color_representation::v1::server::{
                        wp_color_representation_manager_v1::WpColorRepresentationManagerV1,
                        wp_color_representation_surface_v1::WpColorRepresentationSurfaceV1,
                    },
                    wayland_server::{delegate_dispatch, delegate_global_dispatch, Weak, protocol::wl_surface::WlSurface},
                },
                wayland::color::representation::ColorRepresentationState
            };

            delegate_global_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpColorRepresentationManagerV1: ()] => ColorRepresentationState
            );

            delegate_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpColorRepresentationManagerV1: ()] => ColorRepresentationState
            );

            delegate_dispatch!(
                $(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)?
                $ty: [WpColorRepresentationSurfaceV1: Weak<WlSurface>] => ColorRepresentationState
            );
        };
    };
}
