use std::sync::Arc;

use super::*;
use photocraft_color::{ColorMode, SampleType};

fn scene(mode: &str, depth: u32) -> Session {
    let mut session = Session::new();
    session.execute("file.new", json!({"width": 32, "height": 24, "mode": mode, "depth": depth, "background": "transparent"})).unwrap();
    session
}

fn solid_png(width: u32, height: u32, rgba: [u8; 4]) -> String {
    let pixels = rgba.repeat(width as usize * height as usize);
    let image = Image::from_u8(width, height, ChannelLayout::Rgba, pixels).unwrap();
    encoded_png(&image, APPLY).unwrap()
}

fn capture_target(session: &mut Session) -> Value {
    session.execute(CAPTURE, json!({})).unwrap()["target"].clone()
}

#[test]
fn capture_keeps_exact_selection_size_and_reads_visible_merged_pixels() {
    let mut session = scene("rgb", 8);
    session
        .edit("color", |doc, active| {
            doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(Rect::new(9, 7, 13, 11), &[1.0, 0.0, 0.0, 1.0]);
            let mut overlay = Layer::raster("Blue", doc.pixel_format());
            overlay.opacity = 0.5;
            overlay.surface_mut().unwrap().fill_rect(Rect::new(9, 7, 13, 11), &[0.0, 0.0, 1.0, 1.0]);
            doc.insert_above(*active, overlay);
            let mut hidden = Layer::raster("Hidden", doc.pixel_format());
            hidden.visible = false;
            hidden.surface_mut().unwrap().fill_rect(doc.bounds(), &[0.0, 1.0, 0.0, 1.0]);
            doc.insert_above(None, hidden);
            Ok(())
        })
        .unwrap();
    session.execute("select.rect", json!({"x": 5, "y": 3, "width": 14, "height": 12})).unwrap();
    let before = session.active().unwrap().doc.clone();
    let steps = session.active().unwrap().history.past_len();
    let revision = session.active().unwrap().revision;
    let result = session.execute(CAPTURE, json!({})).unwrap();
    assert_eq!(result["target"]["rect"], json!([5, 3, 19, 15]));
    let image = decoded_image(result["reference"].as_str().unwrap(), CAPTURE, false).unwrap();
    assert_eq!(image.dimensions(), (14, 12));
    assert!(image.get(0, 0, 3) < 0.001, "transparent selection margins are retained");
    assert!((image.get(4, 4, 0) - 0.5).abs() < 0.01);
    assert!(image.get(4, 4, 1) < 0.01);
    assert!((image.get(4, 4, 2) - 0.5).abs() < 0.01);
    assert_eq!(session.active().unwrap().history.past_len(), steps);
    assert_eq!(session.active().unwrap().revision, revision);
    assert!(Arc::ptr_eq(&session.active().unwrap().doc, &before));
    assert!(session.clipboard.is_none());
}

#[test]
fn delayed_result_uses_original_area_coverage_and_anchor_in_one_undo() {
    let mut session = scene("rgb", 16);
    let anchor = session.active().unwrap().active_layer.unwrap();
    let area = Rect::new(6, 5, 17, 13);
    session
        .edit("feathered selection", |doc, _| {
            let mut mask = Surface::new(PixelFormat::new(ColorMode::Grayscale, SampleType::F32, false));
            mask.fill_rect(area, &[1.0]);
            mask.fill_rect(Rect::new(6, 5, 8, 13), &[0.5]);
            mask.fill_rect(Rect::new(12, 8, 14, 10), &[0.0]);
            doc.selection = Some(mask);
            Ok(())
        })
        .unwrap();
    let target = capture_target(&mut session);
    session.execute("select.rect", json!({"x": 1, "y": 1, "width": 2, "height": 2})).unwrap();
    session.execute("layer.new.layer", json!({"name": "Changed active layer"})).unwrap();
    let before = session.active().unwrap().doc.clone();
    let steps = session.active().unwrap().history.past_len();
    let result = session.execute(APPLY, json!({"target": target, "data": solid_png(3, 2, [255, 0, 0, 255]), "name": "Generated test"})).unwrap();
    let id = LayerId(result["layer"].as_u64().unwrap());
    let st = session.active().unwrap();
    assert_eq!(st.history.past_len(), steps + 1);
    assert_eq!(st.active_layer, Some(id));
    assert_eq!(st.doc.layers[0].id, anchor);
    assert_eq!(st.doc.layers[1].id, id, "insert above original anchor rather than changed active layer");
    assert_eq!(st.doc.layer(id).unwrap().name, "Generated test");
    assert_eq!(st.doc.layer(id).unwrap().surface().unwrap().content_bounds(), area);
    let surface = st.doc.layer(id).unwrap().surface().unwrap();
    assert!((surface.rgba(6, 5)[3] - 0.5).abs() < 0.0001);
    assert!(surface.rgba(12, 8)[3] < 0.0001, "nonrectangular hole is preserved");
    assert!(surface.rgba(16, 12)[3] > 0.999, "resampling repeats opaque border pixels");
    assert!(surface.rgba(17, 12)[3] < 0.0001);
    assert_eq!(st.doc.selection, before.selection, "current selection is retained");
    assert_eq!(st.doc.layer(anchor), before.layer(anchor), "source pixels are untouched");
    assert!(session.undo());
    assert_eq!(session.active().unwrap().doc, before);
    assert!(session.redo());
    assert!(session.active().unwrap().doc.layer(id).is_some());
}

#[test]
fn capture_and_apply_honor_document_depth_and_color_model() {
    for mode in ["rgb", "gray", "cmyk", "lab"] {
        for depth in [8, 16, 32] {
            let mut session = scene(mode, depth);
            session.execute("select.rect", json!({"x": 4, "y": 6, "width": 9, "height": 7})).unwrap();
            let target = capture_target(&mut session);
            let result = session.execute(APPLY, json!({"target": target, "data": solid_png(9, 7, [160, 160, 160, 255])})).unwrap();
            let id = LayerId(result["layer"].as_u64().unwrap());
            let st = session.active().unwrap();
            let surface = st.doc.layer(id).unwrap().surface().unwrap();
            assert_eq!(surface.format(), st.doc.pixel_format(), "{mode}/{depth}");
            assert_eq!(surface.content_bounds(), Rect::new(4, 6, 13, 13));
            let exported = session.execute(CAPTURE, json!({})).unwrap();
            let image = decoded_image(exported["reference"].as_str().unwrap(), CAPTURE, false).unwrap();
            // CMYK's coating transform and Lab/8 quantization can round-trip by a few levels.
            assert!((image.get(4, 3, 0) - 160.0 / 255.0).abs() < 0.04, "{mode}/{depth}: {:?}", image.to_rgba8());
            assert!(image.get(4, 3, 3) > 0.999);
        }
    }
}

#[test]
fn references_are_converted_from_the_embedded_rgb_profile_to_srgb() {
    let mut session = scene("rgb", 32);
    session
        .edit("P3 color", |doc, active| {
            let bounds = doc.bounds();
            doc.icc_profile = Some(Arc::new(Builtin::DisplayP3.profile().to_bytes().to_vec()));
            doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap().fill_rect(bounds, &[0.6, 0.2, 0.3, 1.0]);
            Ok(())
        })
        .unwrap();
    let result = session.execute(CAPTURE, json!({})).unwrap();
    let image = decoded_image(result["reference"].as_str().unwrap(), CAPTURE, false).unwrap();
    let mut expected = [0.6, 0.2, 0.3, 1.0];
    Transform::new(Builtin::DisplayP3.profile(), Builtin::Srgb.profile(), Intent::RelativeColorimetric, true).unwrap().apply(&mut expected, 4);
    for (channel, expected) in expected.into_iter().enumerate() {
        assert!((image.get(0, 0, channel) - expected.clamp(0.0, 1.0)).abs() <= 1.0 / 255.0);
    }
    // The inverse path places sRGB into P3 storage, while the exported reference remains sRGB.
    session.execute(APPLY, json!({"target": result["target"], "data": solid_png(32, 24, [120, 60, 90, 255])})).unwrap();
    let result = session.execute(CAPTURE, json!({})).unwrap();
    let image = decoded_image(result["reference"].as_str().unwrap(), CAPTURE, false).unwrap();
    assert!((image.get(0, 0, 0) - 120.0 / 255.0).abs() < 0.01);
}

#[test]
fn stale_targets_and_bad_payloads_never_edit_documents() {
    let mut session = scene("rgb", 8);
    let target = capture_target(&mut session);
    let data = solid_png(4, 4, [0, 128, 255, 255]);
    let revision = session.active().unwrap().revision;
    let before = session.active().unwrap().doc.clone();
    let mut bad_rect = target.clone();
    bad_rect["rect"] = json!([i32::MIN, 0, i32::MAX, 1]);
    let mut outside = target.clone();
    outside["rect"] = json!([-1, 0, 3, 4]);
    let mut stale_canvas = target.clone();
    stale_canvas["canvas"] = json!([33, 24]);
    let mut wrong_document = target.clone();
    wrong_document["document"] = json!(u64::MAX);
    let mut gone_anchor = target.clone();
    gone_anchor["anchorLayer"] = json!(u64::MAX);
    let mut bad_coverage = target.clone();
    bad_coverage["coverage"] = json!(solid_png(32, 24, [255, 255, 255, 255]));
    let mut wrong_coverage_size = target.clone();
    let gray = Image::from_u8(1, 1, ChannelLayout::Gray, vec![255]).unwrap();
    wrong_coverage_size["coverage"] = json!(encoded_png(&gray, APPLY).unwrap());
    for params in [
        json!({}),
        json!({"target": null, "data": data}),
        json!({"target": target, "data": "garbage"}),
        json!({"target": target, "data": 9}),
        json!({"target": target, "data": data, "name": 9}),
        json!({"target": target, "data": data, "name": ""}),
        json!({"target": bad_rect, "data": data}),
        json!({"target": outside, "data": data}),
        json!({"target": stale_canvas, "data": data}),
        json!({"target": wrong_document, "data": data}),
        json!({"target": gone_anchor, "data": data}),
        json!({"target": bad_coverage, "data": data}),
        json!({"target": wrong_coverage_size, "data": data}),
    ] {
        assert!(session.execute(APPLY, params.clone()).is_err(), "{params}");
        assert!(Arc::ptr_eq(&session.active().unwrap().doc, &before));
        assert_eq!(session.active().unwrap().revision, revision);
    }
    session.execute("file.new", json!({"width": 32, "height": 24})).unwrap();
    let other = session.active().unwrap().doc.clone();
    assert!(session.execute(APPLY, json!({"target": target, "data": data})).is_err());
    assert!(Arc::ptr_eq(&session.active().unwrap().doc, &other));
    assert!(session.set_active(0));
    session.execute(APPLY, json!({"target": target, "data": data})).unwrap();
    let target = capture_target(&mut session);
    session
        .edit("resize source canvas", |doc, _| {
            doc.size = Size::new(33, 24);
            Ok(())
        })
        .unwrap();
    let before = session.active().unwrap().doc.clone();
    assert!(session.execute(APPLY, json!({"target": target, "data": data})).is_err());
    assert!(Arc::ptr_eq(&session.active().unwrap().doc, &before));
    assert!(session.undo());
    let target = capture_target(&mut session);
    let anchor = session.active().unwrap().active_layer.unwrap();
    session
        .edit("remove source anchor", |doc, active| {
            doc.remove(anchor);
            *active = doc.top_layer();
            Ok(())
        })
        .unwrap();
    let before = session.active().unwrap().doc.clone();
    assert!(session.execute(APPLY, json!({"target": target, "data": data})).is_err());
    assert!(Arc::ptr_eq(&session.active().unwrap().doc, &before));
}

#[test]
fn empty_selection_and_oversized_regions_fail_before_compositing() {
    let mut session = scene("rgb", 8);
    session
        .edit("empty selection", |doc, _| {
            doc.selection = Some(Surface::new(PixelFormat::GRAY8));
            Ok(())
        })
        .unwrap();
    assert!(session.execute(CAPTURE, json!({})).is_err());
    session
        .edit("large canvas", |doc, _| {
            doc.selection = None;
            doc.size = photocraft_geom::Size::new(MAX_SIDE, MAX_SIDE);
            Ok(())
        })
        .unwrap();
    assert!(session.execute(CAPTURE, json!({})).is_err());
    assert!(checked_size(APPLY, 0, 1).is_err());
    assert!(checked_size(APPLY, u32::MAX, u32::MAX).is_err());
}

#[test]
fn command_preconditions_and_allocation_failure_are_graceful() {
    let mut session = Session::new();
    assert!(!session.is_enabled(CAPTURE));
    assert!(!session.is_enabled(APPLY));
    assert!(session.execute(CAPTURE, json!({})).is_err());
    assert!(session.execute(APPLY, json!({})).is_err());
    let mut session = scene("rgb", 8);
    let target = capture_target(&mut session);
    let before = session.active().unwrap().doc.clone();
    crate::allocation::fail_next_for_test();
    assert!(session.execute(APPLY, json!({"target": target, "data": solid_png(4, 4, [255; 4])})).is_err());
    assert!(Arc::ptr_eq(&session.active().unwrap().doc, &before));
    crate::allocation::fail_next_for_test();
    assert!(session.execute(CAPTURE, json!({})).is_err());
    assert!(Arc::ptr_eq(&session.active().unwrap().doc, &before));
}

#[test]
fn entire_image_opens_an_independent_document_at_the_requested_resolution() {
    let mut session = scene("cmyk", 16);
    let original = session.active().unwrap().doc.clone();
    let result = session.execute(OPEN, json!({"data": solid_png(3, 2, [48, 96, 192, 255]), "width": 19, "height": 11, "name": "New scene"})).unwrap();
    assert_eq!(result["document"], 1);
    let doc = &session.active().unwrap().doc;
    assert_eq!(doc.name, "New scene");
    assert_eq!(doc.size, Size::new(19, 11));
    assert_eq!(doc.mode, ColorMode::Rgb);
    assert_eq!(doc.depth, SampleType::U8);
    assert_eq!(doc.layers.len(), 1);
    let surface = doc.layers[0].surface().unwrap();
    assert_eq!(surface.content_bounds(), doc.bounds());
    assert!(surface.rgba(18, 10)[3] > 0.999);
    assert!((surface.rgba(18, 10)[2] - 192.0 / 255.0).abs() < 0.01);
    assert!(Arc::ptr_eq(&session.documents()[0].doc, &original));
    let mut empty = Session::new();
    assert!(empty.is_enabled(OPEN));
    empty.execute(OPEN, json!({"data": solid_png(3, 2, [255; 4])})).unwrap();
    assert_eq!(empty.active().unwrap().doc.size, Size::new(3, 2));
}

#[test]
fn invalid_entire_image_requests_create_no_canvas() {
    let mut session = Session::new();
    let data = solid_png(2, 2, [255; 4]);
    for params in [
        json!({}),
        json!({"data": "bad"}),
        json!({"data": data, "width": 4}),
        json!({"data": data, "width": 4, "height": 0}),
        json!({"data": data, "width": -1, "height": 4}),
        json!({"data": data, "width": u64::MAX, "height": 4}),
        json!({"data": data, "width": MAX_SIDE, "height": MAX_SIDE}),
    ] {
        assert!(session.execute(OPEN, params).is_err());
        assert!(session.documents().is_empty());
    }
}
