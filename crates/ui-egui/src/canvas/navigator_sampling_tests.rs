use super::*;

#[test]
fn navigator_empty_damage_sampling_transition_cannot_leave_proxy_pixels_outside_future_edits() {
    use photocraft_color::{ColorMode, SampleType};
    use photocraft_compose::{render_reduced_damage, thumbnail_buffer, thumbnail_size};
    use photocraft_doc::{Layer, Size};

    // This document crosses the real 16 MP threshold but allocates only a 400x100 pattern.
    // k=5 nearest-neighbour proxy sampling sees white; exact area averaging sees mostly black.
    let mut document = Document::new("sampling transition", Size::new(6000, 4000), ColorMode::Rgb, SampleType::U8);
    let pattern = DRect::new(1000, 1000, 1400, 1100);
    let mut layer = Layer::raster("fine stripes", document.pixel_format());
    let mut pixels = Vec::new();
    for _y in pattern.y0..pattern.y1 {
        for x in pattern.x0..pattern.x1 {
            let value = if x % 5 == 0 { 1.0 } else { 0.0 };
            pixels.extend_from_slice(&[value, value, value, 1.0]);
        }
    }
    layer.surface_mut().unwrap().write_region(pattern, &pixels);
    document.layers.push(layer);
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.session.open_document(document, None);
    app.run("layer.new.layer", json!({})).unwrap();
    let before = app.session.active().unwrap().doc.clone();
    app.run("layer.layerStyle.dropShadow", json!({"distance": 2, "size": 2})).unwrap();
    let state = app.session.active().unwrap();
    let after = &state.doc;
    assert_eq!(state.last_damage, Some(DRect::EMPTY), "styling an empty layer changes no document pixels");
    assert!(photocraft_compose::proxy::proxy_faithful(&before));
    assert!(!photocraft_compose::proxy::proxy_faithful(after));

    let shown = thumbnail_buffer(&before, NAVIGATOR_SIDE);
    let (width, height) = thumbnail_size(after, NAVIGATOR_SIDE);
    // Render only the small pattern's exact output region; a full exact 24 MP composite is
    // unnecessary to prove that unrelated pixels would retain the wrong sampling strategy.
    let exact = render_reduced_damage(after, width, height, pattern);
    assert!(!exact.rect.is_empty());
    let differs_outside_future_edit = exact.px.iter().enumerate().any(|(index, pixel)| {
        let x = exact.rect.x0 as usize + index % exact.rect.width() as usize;
        let y = exact.rect.y0 as usize + index / exact.rect.width() as usize;
        shown.px.get(y * width as usize + x).is_some_and(|old| old != pixel)
    });
    assert!(differs_outside_future_edit, "fixture must reveal aliasing outside a later edit near (3000, 3000)");
    assert!(pattern.intersect(&DRect::new(3000, 3000, 3010, 3010)).is_empty());
    assert!(
        !navigator_patch_compatible(Some(&before), after, DRect::EMPTY),
        "retaining the proxy while advancing to an exact-policy snapshot corrupts a later exact damage update outside its edited area"
    );
}
