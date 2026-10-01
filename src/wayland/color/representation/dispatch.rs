use crate::wayland::{Dispatch2, GlobalData, GlobalDispatch2, compositor};

use super::{
    ColorRepresentationHandler, ColorRepresentationSurfaceCachedState, ColorRepresentationSurfaceData,
};
use wayland_protocols::wp::color_representation::v1::server::{
    wp_color_representation_manager_v1::{self, WpColorRepresentationManagerV1},
    wp_color_representation_surface_v1::{self, WpColorRepresentationSurfaceV1},
};
use wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, New, Resource, Weak, protocol::wl_surface::WlSurface,
};

impl<D> GlobalDispatch2<WpColorRepresentationManagerV1, D> for GlobalData
where
    D: ColorRepresentationHandler,
    D: Dispatch<WpColorRepresentationManagerV1, GlobalData>,
    D: 'static,
{
    fn bind(
        &self,
        state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WpColorRepresentationManagerV1>,
        data_init: &mut DataInit<'_, D>,
    ) {
        let state = state.color_representation_state();
        let instance = data_init.init(resource, GlobalData);

        for (coefficient, range) in &state.coefficients_and_ranges {
            instance.supported_coefficients_and_ranges(*coefficient, *range);
        }
        for mode in &state.alpha_modes {
            instance.supported_alpha_mode(*mode);
        }

        instance.done();

        state.known_instances.push(instance);
    }
}

impl<D> Dispatch2<WpColorRepresentationManagerV1, D> for GlobalData
where
    D: ColorRepresentationHandler,
    D: Dispatch<WpColorRepresentationSurfaceV1, Weak<WlSurface>>,
    D: 'static,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        resource: &WpColorRepresentationManagerV1,
        request: wp_color_representation_manager_v1::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        println!("{:?}", request);
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
        &self,
        state: &mut D,
        _client: wayland_backend::server::ClientId,
        resource: &WpColorRepresentationManagerV1,
    ) {
        let state = state.color_representation_state();
        state.known_instances.retain(|i| i != resource);
    }
}

impl<D> Dispatch2<WpColorRepresentationSurfaceV1, D> for Weak<WlSurface>
where
    D: ColorRepresentationHandler,
    D: 'static,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        resource: &WpColorRepresentationSurfaceV1,
        request: wp_color_representation_surface_v1::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        println!("{:?}", request);
        use wp_color_representation_surface_v1::{Error, Request};
        match request {
            Request::SetAlphaMode { alpha_mode } => {
                let Ok(surface) = self.upgrade() else {
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
                let Ok(surface) = self.upgrade() else {
                    resource.post_error(Error::Inert, "surface doesn't exist");
                    return;
                };

                let wayland_server::WEnum::Value(chroma_location) = chroma_location else {
                    resource.post_error(Error::ChromaLocation, "Unknown chroma location");
                    return;
                };

                let state = state.color_representation_state();
                if !state.chroma_locations.contains(&chroma_location) {
                    resource.post_error(
                        Error::ChromaLocation,
                        "client send chroma location not advertised",
                    );
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
                let Ok(surface) = self.upgrade() else {
                    resource.post_error(Error::Inert, "surface doesn't exist");
                    return;
                };

                let wayland_server::WEnum::Value(coefficients) = coefficients else {
                    resource.post_error(Error::Coefficients, "Unknown coefficients");
                    return;
                };
                let wayland_server::WEnum::Value(range) = range else {
                    resource.post_error(Error::Coefficients, "Unknown range");
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
        &self,
        _state: &mut D,
        _client: wayland_backend::server::ClientId,
        _resource: &WpColorRepresentationSurfaceV1,
    ) {
        let Ok(surface) = self.upgrade() else {
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
