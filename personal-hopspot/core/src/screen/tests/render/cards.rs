use super::*;

#[test]
fn card_stacks_traffic_and_moves_peers_right() {
    let mut display = MockDisplay::new();
    display.set_allow_overdraw(true);
    let card = Card {
        id: InterfaceId::new([0; 8]),
        kind: CardKind::Usb,
        label: card_label("USB"),
        liveness: Liveness::Live,
        failure_reason: None,
        tx_bytes: 123,
        rx_bytes: 456,
        links: 5,
        destinations: 7,
        rate_bytes_per_sec: 12_345,
        last_activity_secs: Some(3),
    };

    draw_card_with_selection(&mut display, 0, &card, false);

    assert_eq!(display.get_pixel(Point::new(4, 14)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(4, 20)), None);
    assert_eq!(display.get_pixel(Point::new(4, 22)), None);
    assert_eq!(display.get_pixel(Point::new(4, 23)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(4, 28)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(4, 29)), None);
    assert_eq!(display.get_pixel(Point::new(33, 14)), None);
    assert_eq!(display.get_pixel(Point::new(37, 14)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(35, 14)), None);
    assert_eq!(display.get_pixel(Point::new(42, 14)), None);
    assert_eq!(display.get_pixel(Point::new(35, 23)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(37, 23)), None);
    assert_eq!(display.get_pixel(Point::new(5, 32)), None);
    assert_eq!(display.get_pixel(Point::new(38, 32)), Some(BinaryColor::On));
}

#[test]
fn large_link_and_peer_counts_fit_right_column() {
    let mut display = MockDisplay::new();
    display.set_allow_overdraw(true);
    let card = Card {
        id: InterfaceId::new([0; 8]),
        kind: CardKind::Wifi,
        label: card_label("Wi-Fi"),
        liveness: Liveness::Live,
        failure_reason: None,
        tx_bytes: 999_999_999,
        rx_bytes: 999_999_999,
        links: 999_999,
        destinations: 1_234_567_890,
        rate_bytes_per_sec: 999_999_999,
        last_activity_secs: Some(3599),
    };

    draw_card_with_selection(&mut display, 0, &card, false);

    assert_eq!(compact_numeric_width("999K"), 20);
    assert_eq!(compact_numeric_width("1.2B"), 17);
    assert!(STAT_TEXT_X + compact_numeric_width("999K") < WIDTH);
    assert!(8 + compact_numeric_width("999M") < STAT_ICON_X);
    assert!(ACTIVITY_TEXT_X + compact_numeric_width("-") < WIDTH);
}

#[test]
fn offline_card_centers_status_and_hides_metrics() {
    let mut display = MockDisplay::new();
    display.set_allow_overdraw(true);
    let card = Card {
        id: InterfaceId::new([0; 8]),
        kind: CardKind::EspNow,
        label: card_label("ESP-NOW"),
        liveness: Liveness::Failed,
        failure_reason: Some("BlueZ GATT Channels >1; set Channels=1"),
        tx_bytes: 123,
        rx_bytes: 456,
        links: 5,
        destinations: 7,
        rate_bytes_per_sec: 123,
        last_activity_secs: Some(12),
    };

    draw_card_with_selection(&mut display, 0, &card, false);

    assert_eq!(display.get_pixel(Point::new(18, 21)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(3, 11)), None);
    assert_eq!(display.get_pixel(Point::new(4, 10)), None);
    assert_eq!(display.get_pixel(Point::new(5, 9)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(3, 4)), None);
    assert_eq!(display.get_pixel(Point::new(4, 14)), None);
    assert_eq!(display.get_pixel(Point::new(44, 14)), None);
    assert_eq!(display.get_pixel(Point::new(45, 23)), None);
    assert_eq!(display.get_pixel(Point::new(5, 32)), None);
    assert_eq!(display.get_pixel(Point::new(36, 32)), None);
}

#[test]
fn home_card_shows_name_and_wraps_the_full_address() {
    let mut display = PanelDisplay::new();
    let identity = test_identity();

    draw_home_card(&mut display, CARD_TOP, &identity, false);

    assert_eq!(
        display.get_pixel(Point::new(0, CARD_TOP)),
        Some(BinaryColor::On)
    );
    assert_eq!(
        display.get_pixel(Point::new(WIDTH - 1, CARD_TOP)),
        Some(BinaryColor::On)
    );
    assert_eq!(
        display.get_pixel(Point::new(0, CARD_TOP + CARD_H - 1)),
        Some(BinaryColor::On)
    );
    assert!(has_on_pixel(
        &display,
        1..WIDTH - 1,
        (CARD_TOP + 3)..(CARD_TOP + 11)
    ));
    for row in 0..3 {
        let row_top = CARD_TOP + HOME_ADDRESS_TOP + row * 8;
        assert!(
            has_on_pixel(&display, HOME_ADDRESS_X..WIDTH - 1, row_top..row_top + 8),
            "address row {row} should carry glyphs"
        );
    }
    assert!(!has_on_pixel(
        &display,
        1..WIDTH - 1,
        (CARD_TOP + HOME_ADDRESS_TOP + 24)..(CARD_TOP + CARD_H - 1)
    ));
}

#[test]
fn selected_home_card_inverts_its_name() {
    let mut display = PanelDisplay::new();
    let identity = test_identity();

    draw_home_card(&mut display, CARD_TOP, &identity, true);

    assert_eq!(
        display.get_pixel(Point::new(1, CARD_TOP + 2)),
        Some(BinaryColor::On)
    );
    let mut inverted_glyph_pixels = false;
    for y in (CARD_TOP + 2)..(CARD_TOP + 12) {
        for x in 1..WIDTH - 1 {
            if display.get_pixel(Point::new(x, y)) == Some(BinaryColor::Off) {
                inverted_glyph_pixels = true;
            }
        }
    }
    assert!(inverted_glyph_pixels);
}

#[test]
fn selected_card_inverts_name_content() {
    let mut display = MockDisplay::new();
    display.set_allow_overdraw(true);
    let card = Card {
        id: InterfaceId::new([0; 8]),
        kind: CardKind::Wifi,
        label: card_label("Wi-Fi"),
        liveness: Liveness::Live,
        failure_reason: None,
        tx_bytes: 0,
        rx_bytes: 0,
        links: 0,
        destinations: 0,
        rate_bytes_per_sec: 0,
        last_activity_secs: None,
    };

    draw_card_with_selection(&mut display, 0, &card, true);

    assert_eq!(display.get_pixel(Point::new(0, 0)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(63, 0)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(0, 11)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(63, 11)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(1, 1)), None);
    assert_eq!(display.get_pixel(Point::new(2, 1)), None);
    assert_eq!(display.get_pixel(Point::new(45, 1)), None);
    assert_eq!(display.get_pixel(Point::new(0, 12)), Some(BinaryColor::On));
    assert_eq!(
        display.get_pixel(Point::new(0, CARD_H - 1)),
        Some(BinaryColor::On)
    );
    assert_eq!(
        display.get_pixel(Point::new(63, CARD_H - 1)),
        Some(BinaryColor::On)
    );
    assert_eq!(
        display.get_pixel(Point::new(31, CARD_H - 1)),
        Some(BinaryColor::On)
    );
    assert_eq!(display.get_pixel(Point::new(2, 2)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(2, 10)), Some(BinaryColor::On));
    assert_eq!(display.get_pixel(Point::new(2, 11)), None);
    assert_eq!(display.get_pixel(Point::new(5, 2)), Some(BinaryColor::Off));
}
