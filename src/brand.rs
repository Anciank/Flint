use gpui_kit::{
    App, IntoElement, ParentElement as _, Styled as _,
    component::{ActiveTheme as _, ColorName},
    div, px, svg,
};

pub const APP_NAME: &str = "Flint";
pub const APP_DISPLAY_TITLE: &str = "Flint IDE";
pub const APP_ID: &str = "dev.flint.ide";
pub const APP_TAGLINE: &str = "AI FIM, Streaming ReAct Explorer, LSP & Vim Code Navigation";

pub const FLINT_LOGO_SVG: &[u8] = include_bytes!("../assets/flint-logo.svg");
pub const FLINT_BLADE_SVG: &[u8] = include_bytes!("../assets/flint-mark-blade.svg");
pub const FLINT_SPARK_SVG: &[u8] = include_bytes!("../assets/flint-mark-spark.svg");

/// Render the multi-layer vector Flint emblem inside a chamfered obsidian squircle badge.
pub fn render_brand_logo(size_px: f32, cx: &App) -> impl IntoElement {
    let inner = (size_px * 0.72).max(11.);
    let radius = (size_px * 0.24).clamp(4., 14.);
    let is_dark = cx.theme().is_dark();
    let badge_bg = if is_dark {
        gpui_kit::rgb(0x0b1220).into()
    } else {
        cx.theme().secondary
    };
    let border_col = ColorName::Cyan.scale(if is_dark { 500 } else { 600 }).opacity(0.45);
    let blade_col = cx.theme().foreground;
    let spark_col = ColorName::Cyan.scale(if is_dark { 400 } else { 600 });

    div()
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .size(px(size_px))
        .rounded(px(radius))
        .bg(badge_bg)
        .border_1()
        .border_color(border_col)
        .child(
            div()
                .relative()
                .size(px(inner))
                .child(
                    svg()
                        .data(FLINT_BLADE_SVG)
                        .size(px(inner))
                        .text_color(blade_col)
                        .absolute()
                        .inset_0(),
                )
                .child(
                    svg()
                        .data(FLINT_SPARK_SVG)
                        .size(px(inner))
                        .text_color(spark_col)
                        .absolute()
                        .inset_0(),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_identity_and_embedded_svg_assets_are_valid() {
        assert_eq!(APP_NAME, "Flint");
        assert_eq!(APP_ID, "dev.flint.ide");
        for bytes in [FLINT_LOGO_SVG, FLINT_BLADE_SVG, FLINT_SPARK_SVG] {
            let text = std::str::from_utf8(bytes).expect("utf-8 SVG asset");
            assert!(text.contains("<svg") && text.contains("<path"));
        }
    }
}
