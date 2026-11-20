use std::rc::Rc;
use std::sync::Arc;

use cst_client::SharedImageHandle;
use gpui::{Image, ImageFormat, ImageSource, ObjectFit, StyledImage as _};
use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement;
use gpui_kit::component::Icon;
use gpui_kit::{
    App, FocusHandle, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, TestSupportExt, Window, div, img, px,
};

use crate::background::{FrostedGlassSpec, render_frosted_surface};
use crate::theme::{
    A11yElementExt as _, DesktopPalette, apply_scale, cjk_inline_label, height, radius, scale,
    space,
};

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
    trap_focus: &FocusHandle,
    close_focus: &FocusHandle,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_close = Rc::new(on_close);
    let close_scrim = Rc::clone(&on_close);
    let close_btn = Rc::clone(&on_close);

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
    .p(space::GROUP)
    .flex()
    .items_center()
    .justify_center()
    .occlude()
    .on_click(move |_, w, cx| close_scrim(w, cx));

    let card = render_frosted_surface(
        "image-lightbox-card",
        FrostedGlassSpec::modal_k3x72(palette, 8.0),
        palette.bg_modal,
        transparency_enabled,
        scale_factor,
    )
    .test_support()
    .a11y_dialog("image-lightbox-card", format!("图片预览：{meta_label}"))
    .max_w(px(960.0))
    .max_h(px(680.0))
    .p(space::LG)
    .rounded(radius::ROW)
    .border_1()
    .border_color(palette.border_subtle)
    .flex()
    .flex_col()
    .gap(space::MD)
    .on_click(|_, _, cx| cx.stop_propagation())
    .child(
        div()
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .gap(space::MD)
            .child(
                apply_scale(div(), scale::UI_MEDIUM)
                    .text_color(palette.text_primary)
                    .truncate()
                    .child(cjk_inline_label(meta_label)),
            )
            .child(
                div()
                    .id("image-lightbox-close-btn")
                    .test_support()
                    .a11y_button("image-lightbox-close-btn", "关闭图片预览")
                    .track_focus(close_focus)
                    .tab_index(0)
                    .w(height::CONTROL)
                    .h(height::CONTROL)
                    .rounded(radius::CONTROL)
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(palette.bg_item_hover))
                    .on_click(move |_, w, cx| close_btn(w, cx))
                    .child(
                        Icon::new(IconName::X)
                            .size(px(16.0))
                            .text_color(palette.text_secondary),
                    ),
            ),
    )
    .child(
        div()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .rounded(radius::ROW)
            .bg(palette.bg_surface)
            .child(
                img(ImageSource::Image(Arc::clone(&target.image)))
                    .max_w(px(912.0))
                    .max_h(px(580.0))
                    .object_fit(ObjectFit::Contain),
            ),
    );

    scrim.child(
        div()
            .tab_stop(false)
            .child(card)
            .focus_trap("image-lightbox-focus-trap", trap_focus),
    )
}
