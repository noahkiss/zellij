use super::*;

fn rgba_image(width: u32, height: u32) -> DecodedImage {
    DecodedImage {
        bytes: vec![0; (width * height * 4) as usize],
        width,
        height,
        format: KittyFormat::Rgba32,
        png: None,
    }
}

#[test]
fn anonymous_images_receive_distinct_monotonically_increasing_internal_ids() {
    let mut store = KittyImageStore::default();
    let id_a = store.store_image(rgba_image(2, 2)).unwrap();
    let id_b = store.store_image(rgba_image(2, 2)).unwrap();
    assert!(id_b > id_a);
    assert_eq!(id_b, id_a + 1);
    assert_eq!(store.image_count(), 2);
    assert_eq!(store.total_bytes(), 32);
}

#[test]
fn lru_zero_refcount_image_is_evicted_first_on_quota_overflow() {
    let mut store = KittyImageStore::with_quota(1000);
    let id_a = store.store_image(rgba_image(10, 10)).unwrap();
    let id_b = store.store_image(rgba_image(10, 10)).unwrap();
    let id_c = store.store_image(rgba_image(10, 10)).unwrap();
    assert!(store.get(id_a).is_none());
    assert!(store.get(id_b).is_some());
    assert!(store.get(id_c).is_some());
    assert_eq!(store.image_count(), 2);
    assert_eq!(store.total_bytes(), 800);
}

#[test]
fn touch_updates_lru_order_for_eviction() {
    let mut store = KittyImageStore::with_quota(1000);
    let id_a = store.store_image(rgba_image(10, 10)).unwrap();
    let id_b = store.store_image(rgba_image(10, 10)).unwrap();
    store.touch(id_a);
    let id_c = store.store_image(rgba_image(10, 10)).unwrap();
    assert!(store.get(id_a).is_some());
    assert!(store.get(id_b).is_none());
    assert!(store.get(id_c).is_some());
    assert_eq!(store.image_count(), 2);
    assert_eq!(store.total_bytes(), 800);
}

#[test]
fn refcounted_image_is_never_evicted_even_when_oldest() {
    let mut store = KittyImageStore::with_quota(1000);
    let id_a = store.store_image(rgba_image(10, 10)).unwrap();
    store.add_placement_ref(id_a);
    let id_b = store.store_image(rgba_image(10, 10)).unwrap();
    let id_c = store.store_image(rgba_image(10, 10)).unwrap();
    assert!(store.get(id_a).is_some());
    assert!(store.get(id_b).is_none());
    assert!(store.get(id_c).is_some());
    assert_eq!(store.total_bytes(), 800);
}

#[test]
fn single_image_exceeding_quota_is_refused_with_einval_and_store_unchanged() {
    let mut store = KittyImageStore::with_quota(1000);
    let id_a = store.store_image(rgba_image(10, 10)).unwrap();
    let result = store.store_image(rgba_image(20, 20));
    match result {
        Err(err) => assert_eq!(err.code, KittyErrorCode::Einval),
        Ok(_) => panic!("expected error"),
    }
    assert_eq!(store.image_count(), 1);
    assert_eq!(store.total_bytes(), 400);
    assert!(store.get(id_a).is_some());
    let id_small = store.store_image(rgba_image(2, 2)).unwrap();
    assert_eq!(id_small, id_a + 1);
}

#[test]
fn free_with_positive_refcount_retains_data_and_removes_at_zero() {
    let mut store = KittyImageStore::default();
    let id_a = store.store_image(rgba_image(10, 10)).unwrap();
    store.add_placement_ref(id_a);
    store.add_placement_ref(id_a);
    assert_eq!(store.refcount(id_a), Some(2));
    store.free(id_a);
    assert_eq!(store.refcount(id_a), Some(1));
    assert!(store.get(id_a).is_some());
    assert_eq!(store.total_bytes(), 400);
    store.free(id_a);
    assert!(store.get(id_a).is_none());
    assert_eq!(store.image_count(), 0);
    assert_eq!(store.total_bytes(), 0);
}

#[test]
fn byte_accounting_matches_hand_computed_total_with_scaled_variants() {
    let mut store = KittyImageStore::default();
    let id_a = store.store_image(rgba_image(10, 10)).unwrap();
    let id_b = store.store_image(rgba_image(4, 5)).unwrap();
    store.add_scaled_variant(id_a, (3, 2), vec![0; 120]);
    store.add_scaled_variant(id_a, (5, 4), vec![0; 60]);
    store.add_scaled_variant(id_a, (3, 2), vec![0; 100]);
    assert_eq!(store.total_bytes(), 640);
    assert_eq!(store.scaled_variant(id_a, (3, 2)).unwrap().len(), 100);
    store.free(id_a);
    assert_eq!(store.total_bytes(), 80);
    store.free(id_b);
    assert_eq!(store.total_bytes(), 0);
}

#[test]
fn remove_placement_ref_keeps_data_but_makes_image_evictable() {
    let mut store = KittyImageStore::with_quota(1000);
    let id_a = store.store_image(rgba_image(10, 10)).unwrap();
    store.add_placement_ref(id_a);
    store.remove_placement_ref(id_a);
    assert!(store.get(id_a).is_some());
    assert_eq!(store.refcount(id_a), Some(0));
    let id_b = store.store_image(rgba_image(10, 10)).unwrap();
    store.add_placement_ref(id_b);
    let id_c = store.store_image(rgba_image(10, 10)).unwrap();
    assert!(store.get(id_a).is_none());
    assert!(store.get(id_b).is_some());
    assert!(store.get(id_c).is_some());
    store.add_placement_ref(id_c);
    let id_d = store.store_image(rgba_image(10, 10)).unwrap();
    assert!(store.get(id_d).is_some());
    assert_eq!(store.total_bytes(), 1200);
}

#[test]
fn rgb24_images_are_normalized_to_rgba_at_store_time() {
    let mut store = KittyImageStore::default();
    let id = store
        .store_image(DecodedImage {
            bytes: (1..=12).collect(),
            width: 2,
            height: 2,
            format: KittyFormat::Rgb24,
            png: None,
        })
        .unwrap();
    let image = store.get(id).unwrap();
    assert_eq!(
        image.rgba,
        vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
    );
    assert_eq!(store.total_bytes(), 16);
}

/// Noise RGBA, which zlib cannot shrink, carrying a stand-in PNG.
fn noisy_image_with_png(width: u32, height: u32, png: Vec<u8>) -> DecodedImage {
    let mut state: u32 = 0x1234_5678;
    let bytes = (0..width * height * 4)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    DecodedImage {
        bytes,
        png: Some(png),
        ..rgba_image(width, height)
    }
}

/// Flat RGBA, which zlib shrinks below any stand-in PNG.
fn flat_image_with_png(width: u32, height: u32, png: Vec<u8>) -> DecodedImage {
    DecodedImage {
        png: Some(png),
        ..rgba_image(width, height)
    }
}

#[test]
fn a_kept_png_counts_against_the_quota_until_the_first_unscaled_transmit() {
    let mut store = KittyImageStore::default();
    let id = store
        .store_image(noisy_image_with_png(10, 10, vec![7; 50]))
        .unwrap();
    assert_eq!(store.total_bytes(), 400 + 50);
    let payload = store.transmit_payload_for(id, None).unwrap();
    assert!(payload.png);
    assert!(!payload.zlib);
    assert_eq!(payload.base64, BASE64_ENCODER.encode(vec![7u8; 50]));
    assert_eq!(store.total_bytes(), 400);
    assert_eq!(store.transmit_payload_for(id, None).unwrap(), payload);
    store.free(id);
    assert_eq!(store.total_bytes(), 0);
}

#[test]
fn a_scaled_transmit_sends_rgba_and_keeps_the_png_for_the_unscaled_one() {
    let mut store = KittyImageStore::default();
    let id = store
        .store_image(noisy_image_with_png(10, 10, vec![7; 50]))
        .unwrap();
    store.add_scaled_variant(id, (1, 1), vec![0; 64]);
    let scaled = store.transmit_payload_for(id, Some((1, 1))).unwrap();
    assert!(!scaled.png);
    assert_eq!(store.total_bytes(), 400 + 50 + 64);
    assert!(store.transmit_payload_for(id, None).unwrap().png);
    assert_eq!(store.total_bytes(), 400 + 64);
}

#[test]
fn a_png_larger_than_the_zlib_payload_is_dropped_for_rgba() {
    let mut store = KittyImageStore::default();
    let id = store
        .store_image(flat_image_with_png(10, 10, vec![7; 300]))
        .unwrap();
    let payload = store.transmit_payload_for(id, None).unwrap();
    assert!(!payload.png);
    assert!(payload.zlib);
    assert_eq!(store.total_bytes(), 400);
}
