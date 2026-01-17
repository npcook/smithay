use crate::wayland::compositor;

use super::{
    ColorRepresentationHandler, ColorRepresentationState, ColorRepresentationSurfaceCachedState,
    ColorRepresentationSurfaceData,
};
use wayland_protocols::wp::color_representation::v1::server::{
    wp_color_representation_manager_v1::{self, WpColorRepresentationManagerV1},
    wp_color_representation_surface_v1::{self, WpColorRepresentationSurfaceV1},
};
use wayland_server::{
    protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
    Resource, Weak,
};

impl<D> GlobalDispatch<WpColorRepresentationManagerV1, (), D> for ColorRepresentationState
where
    D: GlobalDispatch<WpColorRepresentationManagerV1, ()>
        + Dispatch<WpColorRepresentationManagerV1, ()>
        + Dispatch<WpColorRepresentationSurfaceV1, Weak<WlSurface>>
        + ColorRepresentationHandler
        + 'static,
{
    fn bind(
        state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WpColorRepresentationManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        let state = state.color_representation_state();
        let instance = data_init.init(resource, ());

        for (coefficient, range) in &state.coefficients_and_ranges {
            instance.supported_coefficients_and_ranges(*coefficient, *range);
        }
        for mode in &state.alpha_modes {
            instance.supported_alpha_mode(*mode);
        }

        state.known_instances.push(instance);
    }
}

impl<D> Dispatch<WpColorRepresentationManagerV1, (), D> for ColorRepresentationState
where
    D: GlobalDispatch<WpColorRepresentationManagerV1, ()>
        + Dispatch<WpColorRepresentationManagerV1, ()>
        + Dispatch<WpColorRepresentationSurfaceV1, Weak<WlSurface>>
        + ColorRepresentationHandler
        + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        resource: &WpColorRepresentationManagerV1,
        request: wp_color_representation_manager_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        use wp_color_representation_manager_v1::{Error, Request};
        match request {
            Request::GetSurface { id, surface } => {
                compositor::with_states(&surface, |states| {
                    let data = states
                        .data_map
                        .get_or_insert_threadsafe(ColorRepresentationSurfaceData::new);

                    if data.is_resource_attached() {
                        resource.post_error(
                            Error::SurfaceExists,
                            "Surface already has ColorRepresentation attached",
                        );
                        return;
                    }

                    *states
                        .cached_state
                        .get::<ColorRepresentationSurfaceCachedState>()
                        .pending() = ColorRepresentationSurfaceCachedState { ..Default::default() };

                    let instance = data_init.init(id, surface.downgrade());

                    // TODO: add pre_commit_hook to verify chroma_location / coefficient are valid for buffer pixel format

                    *data.instance.lock().unwrap() = Some(instance);
                });
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut D,
        _client: wayland_backend::server::ClientId,
        resource: &WpColorRepresentationManagerV1,
        _data: &(),
    ) {
        let state = state.color_representation_state();
        state.known_instances.retain(|i| i != resource);
    }
}

impl<D> Dispatch<WpColorRepresentationSurfaceV1, Weak<WlSurface>, D> for ColorRepresentationState
where
    D: GlobalDispatch<WpColorRepresentationManagerV1, ()>
        + Dispatch<WpColorRepresentationManagerV1, ()>
        + Dispatch<WpColorRepresentationSurfaceV1, Weak<WlSurface>>
        + ColorRepresentationHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &WpColorRepresentationSurfaceV1,
        request: wp_color_representation_surface_v1::Request,
        data: &Weak<WlSurface>,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        use wp_color_representation_surface_v1::{Error, Request};
        match request {
            Request::SetAlphaMode { alpha_mode } => {
                let Ok(surface) = data.upgrade() else {
                    resource.post_error(Error::Inert, "surface doesn't exist");
                    return;
                };

                let wayland_server::WEnum::Value(alpha_mode) = alpha_mode else {
                    resource.post_error(Error::AlphaMode, "Unknown alpha mode");
                    return;
                };

                compositor::with_states(&surface, |states| {
                    states
                        .cached_state
                        .get::<ColorRepresentationSurfaceCachedState>()
                        .pending()
                        .alpha_mode = Some(alpha_mode);
                });
            }
            Request::SetChromaLocation { chroma_location } => {
                let Ok(surface) = data.upgrade() else {
                    resource.post_error(Error::Inert, "surface doesn't exist");
                    return;
                };

                let wayland_server::WEnum::Value(chroma_location) = chroma_location else {
                    resource.post_error(Error::AlphaMode, "Unknown chroma location");
                    return;
                };

                let state = state.color_representation_state();
                if !state.chroma_locations.contains(&chroma_location) {
                    resource.post_error(Error::Coefficients, "client send chroma location not advertised");
                    return;
                }

                compositor::with_states(&surface, |states| {
                    states
                        .cached_state
                        .get::<ColorRepresentationSurfaceCachedState>()
                        .pending()
                        .chroma_location = Some(chroma_location);
                });
            }
            Request::SetCoefficientsAndRange { coefficients, range } => {
                let Ok(surface) = data.upgrade() else {
                    resource.post_error(Error::Inert, "surface doesn't exist");
                    return;
                };

                let wayland_server::WEnum::Value(coefficients) = coefficients else {
                    resource.post_error(Error::AlphaMode, "Unknown coefficients");
                    return;
                };
                let wayland_server::WEnum::Value(range) = range else {
                    resource.post_error(Error::AlphaMode, "Unknown range");
                    return;
                };

                let coefficients_and_range = (coefficients, range);

                let state = state.color_representation_state();
                if !state.coefficients_and_ranges.contains(&coefficients_and_range) {
                    resource.post_error(Error::Coefficients, "client send coefficient not advertised");
                    return;
                }

                compositor::with_states(&surface, |states| {
                    states
                        .cached_state
                        .get::<ColorRepresentationSurfaceCachedState>()
                        .pending()
                        .coefficients_and_range = Some(coefficients_and_range);
                });
            }
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: wayland_backend::server::ClientId,
        _resource: &WpColorRepresentationSurfaceV1,
        data: &Weak<WlSurface>,
    ) {
        let Ok(surface) = data.upgrade() else {
            return;
        };

        compositor::with_states(&surface, |states| {
            if let Some(data) = states.data_map.get::<ColorRepresentationSurfaceData>() {
                data.instance.lock().unwrap().take();
            }

            *states
                .cached_state
                .get::<ColorRepresentationSurfaceCachedState>()
                .pending() = ColorRepresentationSurfaceCachedState::default();
        });
    }
}
