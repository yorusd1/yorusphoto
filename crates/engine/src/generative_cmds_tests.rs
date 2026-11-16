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

/// An animated GIF whose frames are solid colours, `delay_cs` hundredths of a second each.
fn animated_gif(width: u32, height: u32, colours: &[[u8; 3]], delay_cs: u16) -> String {
    let frames: Vec<photocraft_codecs::web::GifFrame> = colours
        .iter()
        .map(|rgb| photocraft_codecs::web::GifFrame { indices: vec![0; (width * height) as usize], palette: vec![*rgb], transparent: None, delay_cs })
        .collect();
    STANDARD.encode(photocraft_codecs::web::encode_gif_animated(width, height, &frames, true).unwrap())
}

#[test]
fn grow_fit_enlarges_the_document_so_a_bigger_result_keeps_its_pixels() {
    let mut session = scene("rgb", 8);
    session.execute("select.rect", json!({"x": 8, "y": 6, "width": 8, "height": 6})).unwrap();
    let target = capture_target(&mut session);
    let steps = session.active().unwrap().history.past_len();
    // 4x the selection's pixels on both axes: the whole document grows 4x.
    let result = session.execute(APPLY, json!({"target": target, "data": solid_png(32, 24, [0, 200, 0, 255]), "fit": "grow"})).unwrap();
    let st = session.active().unwrap();
    assert_eq!(st.doc.size, Size::new(128, 96), "the document grew by the result's scale");
    assert_eq!(st.history.past_len(), steps + 2, "Image Size, then the placement");
    let id = LayerId(result["layer"].as_u64().unwrap());
    assert_eq!(st.doc.layer(id).unwrap().surface().unwrap().content_bounds(), Rect::new(32, 24, 64, 48), "same place and size in the picture");
    assert_eq!(result["grown"]["width"], 128);
    assert_eq!(result["target"]["canvas"], json!([128, 96]));
    assert_eq!(result["target"]["rect"], json!([32, 24, 64, 48]));
    // The returned target places the request's next result on the grown document without growing again.
    let next = session.execute(APPLY, json!({"target": result["target"], "data": solid_png(32, 24, [0, 0, 200, 255]), "fit": "grow"})).unwrap();
    assert!(next.get("grown").is_none());
    assert_eq!(session.active().unwrap().doc.size, Size::new(128, 96));
    // The original target no longer matches the grown canvas.
    assert!(session.execute(APPLY, json!({"target": target, "data": solid_png(8, 6, [9, 9, 9, 255])})).is_err());
}

#[test]
fn exact_fit_is_the_default_and_growth_is_bounded() {
    let mut session = scene("rgb", 8);
    session.execute("select.rect", json!({"x": 8, "y": 6, "width": 8, "height": 6})).unwrap();
    let target = capture_target(&mut session);
    let result = session.execute(APPLY, json!({"target": target, "data": solid_png(32, 24, [0, 200, 0, 255])})).unwrap();
    assert!(result.get("grown").is_none());
    assert_eq!(session.active().unwrap().doc.size, Size::new(32, 24));
    assert!(session.execute(APPLY, json!({"target": target, "data": solid_png(4, 4, [1, 1, 1, 255]), "fit": "stretch"})).is_err());
    // A smaller result never shrinks anything; a huge one is capped by side and by pixels.
    assert_eq!(growth_scale([32, 24], Rect::new(0, 0, 32, 24), (16, 12)), 1.0);
    let capped = growth_scale([8000, 8000], Rect::new(0, 0, 100, 100), (4000, 4000));
    assert!(capped > 1.0 && capped * 8000.0 <= f64::from(MAX_GROWN_SIDE) + 0.5 && capped * capped * 64e6 <= MAX_GROWN_PIXELS as f64 + 1.0, "{capped}");
    assert_eq!(growth_scale([MAX_GROWN_SIDE, 10], Rect::new(0, 0, 10, 10), (100, 100)), 1.0, "already at the side limit");
}

#[test]
fn an_animated_gif_places_one_layer_per_frame_with_only_the_first_shown() {
    let mut session = scene("rgb", 8);
    session.execute("select.rect", json!({"x": 4, "y": 4, "width": 8, "height": 8})).unwrap();
    let target = capture_target(&mut session);
    let steps = session.active().unwrap().history.past_len();
    let gif = animated_gif(8, 8, &[[255, 0, 0], [0, 255, 0], [0, 0, 255]], 10);
    let result = session.execute(APPLY, json!({"target": target, "data": gif, "name": "Slash"})).unwrap();
    assert_eq!(result["frames"], 3);
    assert_eq!(result["delays"], json!([100, 100, 100]));
    let st = session.active().unwrap();
    assert_eq!(st.history.past_len(), steps + 1, "one undo step for the whole animation");
    let ids: Vec<LayerId> = result["layers"].as_array().unwrap().iter().map(|id| LayerId(id.as_u64().unwrap())).collect();
    assert_eq!(ids.len(), 3);
    for (index, id) in ids.iter().enumerate() {
        let layer = st.doc.layer(*id).unwrap();
        assert_eq!(layer.visible, index == 0, "frame {index}");
        assert!(layer.name.starts_with(&format!("Slash · frame {} of 3", index + 1)), "{}", layer.name);
        assert_eq!(layer.surface().unwrap().content_bounds(), Rect::new(4, 4, 12, 12));
    }
    let colour = |index: usize| st.doc.layer(ids[index]).unwrap().surface().unwrap().rgba(6, 6);
    assert!(colour(0)[0] > 0.9 && colour(1)[1] > 0.9 && colour(2)[2] > 0.9, "frames keep their order bottom to top");
    let order: Vec<LayerId> = st.doc.layers.iter().map(|layer| layer.id).collect();
    let position = |id: &LayerId| order.iter().position(|other| other == id).unwrap();
    assert!(position(&ids[0]) < position(&ids[1]) && position(&ids[1]) < position(&ids[2]));
}

/// A 2 x 2 flipbook sheet drawn as one still: red, green, blue, yellow cells of `cell` pixels.
fn quadrant_sheet(cell: u32) -> String {
    let side = cell * 2;
    let mut pixels = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            pixels.extend(match (x < cell, y < cell) {
                (true, true) => [255, 0, 0, 255],
                (false, true) => [0, 255, 0, 255],
                (true, false) => [0, 0, 255, 255],
                (false, false) => [255, 255, 0, 255],
            });
        }
    }
    encoded_png(&Image::from_u8(side, side, ChannelLayout::Rgba, pixels).unwrap(), APPLY).unwrap()
}

#[test]
fn a_still_flipbook_sheet_with_a_grid_becomes_its_frames_in_reading_order() {
    let mut session = scene("rgb", 8);
    session.execute("select.rect", json!({"x": 4, "y": 4, "width": 8, "height": 8})).unwrap();
    let target = capture_target(&mut session);
    let grid = json!({"columns": 2, "rows": 2, "count": 3});
    let result = session.execute(APPLY, json!({"target": target, "data": quadrant_sheet(8), "name": "Flame", "grid": grid})).unwrap();
    assert_eq!(result["frames"], 3, "the fourth cell is left out");
    let st = session.active().unwrap();
    let ids: Vec<LayerId> = result["layers"].as_array().unwrap().iter().map(|id| LayerId(id.as_u64().unwrap())).collect();
    let colour = |index: usize| st.doc.layer(ids[index]).unwrap().surface().unwrap().rgba(6, 6);
    assert!(colour(0)[0] > 0.9 && colour(0)[1] < 0.1, "red first");
    assert!(colour(1)[1] > 0.9 && colour(1)[0] < 0.1, "then green, the rest of the first row");
    assert!(colour(2)[2] > 0.9, "then blue, the second row");
    for id in &ids {
        assert_eq!(st.doc.layer(*id).unwrap().surface().unwrap().content_bounds(), Rect::new(4, 4, 12, 12));
    }
    // A new canvas cuts the sheet the same way; an animation or a still with no grid is left whole.
    let opened = session.execute(OPEN, json!({"data": quadrant_sheet(4), "grid": {"columns": 2, "rows": 2}})).unwrap();
    assert_eq!(opened["frames"], 4);
    assert_eq!((session.active().unwrap().doc.size, session.active().unwrap().doc.layers.len()), (Size::new(4, 4), 4));
    let whole = session.execute(OPEN, json!({"data": quadrant_sheet(4)})).unwrap();
    assert_eq!(whole["frames"], 1);
    let gif = animated_gif(4, 4, &[[255, 0, 0], [0, 255, 0]], 10);
    assert_eq!(session.execute(OPEN, json!({"data": gif, "grid": {"columns": 2, "rows": 2}})).unwrap()["frames"], 2, "a real animation keeps its frames");
    for bad in [json!({"columns": 0, "rows": 2}), json!({"columns": 2}), json!({"columns": 2, "rows": 2, "count": 5}), json!({"columns": 17, "rows": 1})] {
        assert!(session.execute(OPEN, json!({"data": quadrant_sheet(4), "grid": bad})).is_err(), "{bad}");
    }
    assert!(session.execute(OPEN, json!({"data": solid_png(3, 3, [0, 0, 0, 255]), "grid": {"columns": 4, "rows": 4}})).is_err(), "more cells than pixels");
}

#[test]
fn an_animation_opens_as_frame_layers_or_as_a_flipbook_sheet() {
    let gif = animated_gif(4, 3, &[[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0], [0, 255, 255]], 5);
    let mut session = Session::new();
    let layers = session.execute(OPEN, json!({"data": gif, "name": "Smoke"})).unwrap();
    assert_eq!(layers["frames"], 5);
    let doc = &session.active().unwrap().doc;
    assert_eq!((doc.size, doc.layers.len()), (Size::new(4, 3), 5));
    assert!(doc.layers[0].visible && !doc.layers[4].visible);
    let sheet = session.execute(OPEN, json!({"data": gif, "name": "Smoke", "frames": "sheet"})).unwrap();
    assert_eq!(sheet["sheet"]["columns"], 3, "five frames fill the smallest square grid, 3x3");
    assert_eq!(sheet["sheet"]["rows"], 2);
    let doc = &session.active().unwrap().doc;
    assert_eq!((doc.size, doc.layers.len()), (Size::new(12, 6), 1));
    let surface = doc.layers[0].surface().unwrap();
    assert!(surface.rgba(1, 1)[0] > 0.9, "frame 1 at the top left");
    assert!(surface.rgba(9, 1)[2] > 0.9, "frame 3 ends the first row");
    assert!(surface.rgba(5, 4)[1] > 0.9 && surface.rgba(5, 4)[2] > 0.9, "frame 5 in the second row");
    assert!(surface.rgba(9, 4)[3] < 0.01, "the unused cell stays empty");
    let four = session.execute(OPEN, json!({"data": gif, "frames": "sheet", "columns": 5})).unwrap();
    assert_eq!((four["width"].as_u64(), four["height"].as_u64()), (Some(20), Some(3)));
    assert!(session.execute(OPEN, json!({"data": gif, "frames": "grid"})).is_err());
    assert!(session.execute(OPEN, json!({"data": gif, "frames": "sheet", "columns": 0})).is_err());
}
