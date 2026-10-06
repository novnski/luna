//! [`frosted`] — the frosted-glass float: wraps a popover/dialog card so its
//! ENTIRE subtree paints inside one scene layer (a single draw order) with a
//! backdrop blur painted first.
//!
//! The single layer order is the point: with per-primitive bounds-tree
//! ordering, a hover repaint elsewhere could reassign the card's quads BELOW
//! the blur — washes, dividers, and borders intermittently got snapshotted and
//! blurred away (user reports). Inside one layer the blur/content relationship
//! is structural: blur first, then shadow, tint, border, rows, text.

use gpui::{
    AnyElement, App, BorderStyle, Bounds, Corners, Element, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, Window, hsla, point, px,
};

use crate::theme::Theme;

/// Shared backdrop-blur sigma for floating menus, popovers and palettes.
/// Keep these on the same surface as Cmd+K and the composer.
pub const MENU_BLUR: f32 = 16.0;

/// Frost `child` (a popover card): backdrop-blurred on glass, pass-through on
/// opaque platforms. `corner_radius` must match the card's rounding.
pub fn frosted(corner_radius: f32, blur_radius: f32, child: impl IntoElement) -> Frosted {
    Frosted {
        corner_radius,
        blur_radius,
        child: child.into_any_element(),
        elevated: false,
    }
}

pub struct Frosted {
    corner_radius: f32,
    blur_radius: f32,
    child: AnyElement,
    elevated: bool,
}

impl Frosted {
    /// Give a floating card its elevation on glass (audit CP-16). A box
    /// shadow can't be used: GPUI fills it beneath the element and the
    /// translucent card shows it as a stain. Instead soft rings are painted
    /// OUTSIDE the card, after its blur, so the glass interior stays clean.
    /// Opaque themes keep their cards' own [`crate::theme::menu_shadows`].
    pub fn elevated(mut self) -> Self {
        self.elevated = true;
        self
    }
}

/// Rings outside a frosted card: a crisp contact rim against its hairline,
/// then a soft falloff that drifts downward like an ambient drop shadow.
/// Painted with the window's element opacity, so they fade with the card.
fn paint_elevation(window: &mut Window, bounds: Bounds<Pixels>, corner_radius: f32) {
    const RINGS: usize = 8;
    let (contact, ambient) = match crate::theme::current_appearance() {
        crate::theme::Appearance::Dark => (0.42, 0.20),
        crate::theme::Appearance::Light => (0.10, 0.055),
    };
    for ring in 1..=RINGS {
        let distance = ring as f32;
        let alpha = if ring == 1 {
            contact
        } else {
            let falloff = 1.0 - (distance - 1.0) / RINGS as f32;
            ambient * falloff * falloff
        };
        // Each ring sits strictly outside the card: its downward drift stays
        // under its distance.
        let drift = (distance - 1.0) * 0.45;
        let ring_bounds = bounds.dilate(px(distance));
        let ring_bounds = Bounds {
            origin: point(ring_bounds.origin.x, ring_bounds.origin.y + px(drift)),
            size: ring_bounds.size,
        };
        window.paint_quad(gpui::quad(
            ring_bounds,
            Corners::all(px(corner_radius + distance)),
            gpui::transparent_black(),
            px(1.0),
            hsla(0.0, 0.0, 0.0, alpha),
            BorderStyle::Solid,
        ));
    }
}

impl Element for Frosted {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if Theme::of(cx).is_frost() {
            window.paint_layer(bounds, |window| {
                window.paint_backdrop_blur(
                    bounds,
                    Corners::all(px(self.corner_radius)),
                    px(self.blur_radius),
                );
                self.child.paint(window, cx);
            });
            if self.elevated {
                paint_elevation(window, bounds, self.corner_radius);
            }
        } else {
            self.child.paint(window, cx);
        }
    }
}

impl IntoElement for Frosted {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// Paint `child` in its own scene layer, giving it a fresh draw order above
/// everything painted so far in the enclosing layer.
///
/// Needed for overlays INSIDE a frosted card: the card's single layer means
/// every primitive shares one draw order, and equal orders render grouped by
/// primitive kind (quads, then icons, then images) — so a close button's
/// circle painted "after" a thumbnail still shows up UNDER the image. A
/// nested layer restores the intended stacking.
pub fn layered(child: impl IntoElement) -> Layered {
    Layered {
        child: child.into_any_element(),
    }
}

pub struct Layered {
    child: AnyElement,
}

impl Element for Layered {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.paint_layer(bounds, |window| self.child.paint(window, cx));
    }
}

impl IntoElement for Layered {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
