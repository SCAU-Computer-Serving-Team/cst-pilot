use std::rc::Rc;
use std::sync::Arc;

use cst_client::SharedImageHandle;
use gpui::{Image, ImageFormat, ImageSource, ObjectFit, Pixels, Size, StyledImage as _};
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, FocusHandle, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, img, px,
};

use crate::accessibility::A11yElementExt as _;
use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{DesktopPalette, height, motion, radius, space};

#[derive(Debug, Clone)]
pub struct LightboxImageTarget {
    pub title: SharedString,
    pub dimensions: Option<(u32, u32)>,
    pub image: Arc<Image>,
    pub prior_focus: Option<FocusHandle>,
}

pub fn gpui_image_from_bytes(mime_type: &str, bytes: Vec<u8>) -> Arc<Image> {
    let format = ImageFormat::from_mime_type(mime_type).unwrap_or(ImageFormat::Png);
    Arc::new(Image::from_bytes(format, bytes))
}

pub fn gpui_image_from_handle(handle: &SharedImageHandle) -> Arc<Image> {
    gpui_image_from_bytes(handle.mime_type(), handle.compressed_bytes().to_vec())
}

/// Full-window Tier 3 frosted-glass lightbox modal (`K3x72` + `DZ0Fp` backdrop) for original-aspect
/// preview of composer draft thumbnails, message attachments, and `read` tool images.
#[allow(clippy::too_many_arguments)]
pub fn render_image_lightbox_modal(
    target: &LightboxImageTarget,
    palette: &DesktopPalette,
    transparency_enabled: bool,
    scale_factor: f32,
    viewport_size: Size<Pixels>,
    trap_focus: &FocusHandle,
    close_focus: &FocusHandle,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_close = Rc::new(on_close);
    let close_scrim = Rc::clone(&on_close);
    let close_btn = Rc::clone(&on_close);
    let close_img = Rc::clone(&on_close);

    let meta_label = match target.dimensions {
        Some((w, h)) => SharedString::from(format!("{} · {w}×{h}", target.title)),
        None => target.title.clone(),
    };

    let scrim = render_frosted_surface(
        "image-lightbox-scrim",
        FrostedGlassSpec::scrim_dz0fp(palette),
        palette.bg_scrim,
        transparency_enabled,
        scale_factor,
    )
    .test_support()
    .absolute()
    .top_0()
    .left_0()
    .right_0()
    .bottom_0()
    .flex()
    .items_center()
    .justify_center()
    .occlude()
    .on_click(move |_, w, cx| close_scrim(w, cx));

    let max_image_w = (f32::from(viewport_size.width) - 58.0).clamp(160.0, 960.0);
    let max_image_h = (f32::from(viewport_size.height) - 98.0).clamp(160.0, 680.0);

    let close_circle_btn = div()
        .id("image-lightbox-close-btn")
        .test_support()
        .a11y_button("image-lightbox-close-btn", "关闭图片预览")
        .track_focus(close_focus)
        .tab_index(0)
        .absolute()
        .top(space::GROUP)
        .right(space::GROUP)
        .w(height::FIELD)
        .h(height::FIELD)
        .rounded(radius::FULL)
        .bg(palette.bg_elevated)
        .border_1()
        .border_color(palette.border_subtle)
        .shadow(palette.craft_lift_shadows())
        .flex()
        .items_center()
        .justify_center()
        .hover(|s| s.bg(palette.bg_surface))
        .on_click(move |_, w, cx| {
            cx.stop_propagation();
            close_btn(w, cx);
        })
        .child(
            Icon::new(IconName::X)
                .size(px(22.0))
                .text_color(palette.text_secondary),
        );

    let image_frame = div()
        .id("image-lightbox-body")
        .max_w_full()
        .max_h_full()
        .min_w_0()
        .min_h_0()
        .rounded(radius::ROW)
        .border_1()
        .border_color(palette.border_subtle)
        .bg(palette.bg_surface)
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .on_click(move |_, w, cx| {
            cx.stop_propagation();
            close_img(w, cx);
        })
        .child(
            img(ImageSource::Image(Arc::clone(&target.image)))
                .max_w(px(max_image_w))
                .max_h(px(max_image_h))
                .object_fit(ObjectFit::Contain),
        );

    scrim.child(
        div()
            .id("image-lightbox-card")
            .test_support()
            .a11y_dialog("image-lightbox-card", format!("图片预览：{meta_label}"))
            .relative()
            .size_full()
            .p(space::GROUP)
            .flex()
            .items_center()
            .justify_center()
            .tab_stop(false)
            .child(motion::animate_modal_enter(
                "image-lightbox-anim",
                image_frame,
                Vec::new(),
            ))
            .child(close_circle_btn)
            .focus_trap("image-lightbox-focus-trap", trap_focus),
    )
}
