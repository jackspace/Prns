use super::*;

#[test]
fn short_press_cycles_global_then_cards_and_pages_visible_window() {
    let cards = test_cards::<5>(CardKind::Usb);
    let content = test_content(&cards);
    let mut state = test_ui_state();
    state.sync(content);

    assert!(state.global_selected());
    assert_eq!(state.selected_card_index(content), None);
    assert_eq!(state.visible_start, 0);

    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.selected_card_index(content), Some(0));
    assert_eq!(state.visible_start, 0);

    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.selected_card_index(content), Some(1));
    assert_eq!(state.visible_start, 0);

    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.selected_card_index(content), Some(2));
    assert_eq!(state.visible_start, 2);

    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.selected_card_index(content), Some(3));
    assert_eq!(state.visible_start, 3);

    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.selected_card_index(content), Some(4));
    assert_eq!(state.visible_start, 4);

    state.handle_input(InputEvent::ShortPress, content);
    assert!(state.global_selected());
    assert_eq!(state.selected_card_index(content), None);
    assert_eq!(state.visible_start, 0);
}

#[test]
fn long_press_opens_global_menu_and_short_press_cycles_menu_items() {
    let cards = test_cards::<4>(CardKind::Usb);
    let content = test_content(&cards);
    let mut state = test_ui_state();

    state.handle_input(InputEvent::LongPress, content);

    assert_eq!(state.selected_card_index(content), None);
    assert_eq!(state.visible_start, 0);
    assert_eq!(state.global_menu_selected_item(), Some(0));

    state.handle_input(InputEvent::ShortPress, content);

    assert_eq!(state.selected_card_index(content), None);
    assert_eq!(state.global_menu_selected_item(), Some(1));

    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.global_menu_selected_item(), Some(2));
    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.global_menu_selected_item(), Some(3));

    state.handle_input(InputEvent::LongPress, content);

    assert!(state.global_selected());
}

#[test]
fn long_press_on_the_announce_item_returns_the_announce_action() {
    let cards = test_cards::<4>(CardKind::Usb);
    let content = test_content(&cards);
    let mut state = test_ui_state();

    assert_eq!(
        state.handle_input(InputEvent::LongPress, content),
        UiAction::None
    );
    assert_eq!(state.global_menu_selected_item(), Some(ANNOUNCE_MENU_ITEM));

    assert_eq!(
        state.handle_input(InputEvent::LongPress, content),
        UiAction::Announce,
    );
    assert!(state.global_selected());
}

#[test]
fn long_press_on_limits_opens_the_paged_limits_page() {
    let cards = test_cards::<4>(CardKind::Usb);
    let content = test_content(&cards);
    let mut state = test_ui_state();
    state.handle_input(InputEvent::LongPress, content);
    state.handle_input(InputEvent::ShortPress, content);

    assert_eq!(
        state.handle_input(InputEvent::LongPress, content),
        UiAction::None
    );
    assert_eq!(state.mode, UiMode::LimitsPage { page: 0 });
    assert_eq!(
        state.handle_input(InputEvent::ShortPress, content),
        UiAction::None
    );
    assert_eq!(state.mode, UiMode::LimitsPage { page: 1 });
    assert_eq!(
        state.handle_input(InputEvent::LongPress, content),
        UiAction::None
    );
    assert!(state.global_selected());
}

#[test]
fn long_press_on_sleep_enters_sleep_and_next_press_wakes() {
    let cards = test_cards::<4>(CardKind::Usb);
    let content = test_content(&cards);
    let mut state = test_ui_state();
    state.handle_input(InputEvent::LongPress, content);
    state.handle_input(InputEvent::ShortPress, content);
    state.handle_input(InputEvent::ShortPress, content);

    assert_eq!(
        state.handle_input(InputEvent::LongPress, content),
        UiAction::Sleep
    );
    assert_eq!(state.mode, UiMode::Sleeping);
    assert_eq!(
        state.handle_input(InputEvent::ShortPress, content),
        UiAction::Wake
    );
    assert!(state.global_selected());
}

#[test]
fn oled_capable_menu_offers_display_off_before_sleep() {
    let cards = test_cards::<4>(CardKind::Usb);
    let content = test_content(&cards);
    let mut state = test_ui_state_with_display_power();
    state.handle_input(InputEvent::LongPress, content);
    state.handle_input(InputEvent::ShortPress, content);
    state.handle_input(InputEvent::ShortPress, content);

    assert_eq!(state.global_menu_selected_item(), Some(OLED_OFF_MENU_ITEM));
    assert_eq!(
        state.handle_input(InputEvent::LongPress, content),
        UiAction::OledOff
    );
    assert!(state.global_selected());

    state.handle_input(InputEvent::LongPress, content);
    for _ in 0..SLEEP_MENU_ITEM {
        state.handle_input(InputEvent::ShortPress, content);
    }
    assert_eq!(
        state.handle_input(InputEvent::LongPress, content),
        UiAction::Sleep
    );
}

#[test]
fn long_press_on_back_closes_the_global_menu() {
    let cards = test_cards::<4>(CardKind::Usb);
    let content = test_content(&cards);
    let mut state = test_ui_state();
    state.handle_input(InputEvent::LongPress, content);
    for _ in 0..3 {
        state.handle_input(InputEvent::ShortPress, content);
    }

    assert_eq!(
        state.handle_input(InputEvent::LongPress, content),
        UiAction::None
    );
    assert!(state.global_selected());
}

#[test]
fn global_menu_cycles_only_actionable_items() {
    let cards = test_cards::<1>(CardKind::Usb);
    let content = test_content(&cards);
    let mut state = test_ui_state();
    state.handle_input(InputEvent::LongPress, content);

    assert_eq!(state.global_menu_selected_item(), Some(0));
    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.global_menu_selected_item(), Some(1));
    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.global_menu_selected_item(), Some(2));
    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.global_menu_selected_item(), Some(3));
    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.global_menu_selected_item(), Some(0));
}

#[test]
fn supported_access_point_states_offer_the_radio_swap_action() {
    let cards = test_cards::<1>(CardKind::Usb);
    let content = test_content(&cards);
    for access_point in [AccessPointState::Inactive, AccessPointState::Active] {
        let mut state = test_ui_state_with_access_point(access_point);
        state.handle_input(InputEvent::LongPress, content);
        for _ in 0..RADIO_MENU_ITEM_NO_DISPLAY {
            state.handle_input(InputEvent::ShortPress, content);
        }

        assert_eq!(
            state.global_menu_selected_item(),
            Some(RADIO_MENU_ITEM_NO_DISPLAY)
        );
        assert_eq!(
            state.handle_input(InputEvent::LongPress, content),
            UiAction::None
        );
        assert_eq!(state.mode, UiMode::ConfirmRadioSwap { confirm: false });
        state.handle_input(InputEvent::ShortPress, content);
        assert_eq!(
            state.handle_input(InputEvent::LongPress, content),
            UiAction::SwapRadioMode
        );
    }
}

#[test]
fn non_lora_interface_menus_cycle_power_and_back_only() {
    let cards = test_cards::<1>(CardKind::Usb);
    let content = test_content(&cards);
    let mut state = test_ui_state();
    state.handle_input(InputEvent::ShortPress, content);
    state.handle_input(InputEvent::LongPress, content);

    assert_eq!(state.interface_menu_selected_item(), Some(0));
    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.interface_menu_selected_item(), Some(1));
    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.interface_menu_selected_item(), Some(0));
}

#[test]
fn lora_interface_menu_keeps_tune_and_reset() {
    let cards = test_cards::<1>(CardKind::LoRa);
    let content = test_content(&cards);
    let mut state = test_ui_state();
    state.handle_input(InputEvent::ShortPress, content);
    state.handle_input(InputEvent::LongPress, content);

    assert_eq!(state.interface_menu_selected_item(), Some(0));
    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(
        state.interface_menu_selected_item(),
        Some(LORA_TUNE_MENU_ITEM)
    );
    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(
        state.interface_menu_selected_item(),
        Some(LORA_RESET_MENU_ITEM)
    );
    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.interface_menu_selected_item(), Some(3));
    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.interface_menu_selected_item(), Some(0));
}

#[test]
fn home_card_sits_between_global_row_and_first_interface_card() {
    let cards = test_cards::<2>(CardKind::Usb);
    let identity = test_identity();
    let local_docs = LocalDocsAccess {
        wifi_ssid: "Hopspot-EW53",
        docs_host: "192.168.4.1",
    };
    let content = ScreenContent {
        cards: &cards,
        node_identity: Some(&identity),
        local_docs: Some(&local_docs),
    };
    let mut state = test_ui_state();
    state.sync(content);

    assert!(state.global_selected());
    state.handle_input(InputEvent::ShortPress, content);
    assert!(state.home_selected(content));
    assert_eq!(state.selected_card_index(content), None);

    assert_eq!(
        state.handle_input(InputEvent::LongPress, content),
        UiAction::None
    );
    assert_eq!(state.interface_menu_selected_item(), None);

    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.selected_card_index(content), Some(0));
    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.selected_card_index(content), Some(1));

    state.handle_input(InputEvent::ShortPress, content);
    assert_eq!(state.selected_card_index(content), None);
    assert_eq!(
        state.handle_input(InputEvent::LongPress, content),
        UiAction::OpenDocs
    );
}

#[test]
fn long_press_opens_interface_menu_after_card_focus() {
    let cards = test_cards::<4>(CardKind::Usb);
    let content = test_content(&cards);
    let mut state = test_ui_state();
    state.handle_input(InputEvent::ShortPress, content);

    state.handle_input(InputEvent::LongPress, content);

    assert_eq!(state.selected_card_index(content), Some(0));
    assert_eq!(state.visible_start, 0);
    assert_eq!(state.interface_menu_selected_item(), Some(0));

    state.handle_input(InputEvent::ShortPress, content);

    assert_eq!(state.selected_card_index(content), Some(0));
    assert_eq!(state.interface_menu_selected_item(), Some(1));

    state.handle_input(InputEvent::LongPress, content);

    assert_eq!(state.selected_card_index(content), Some(0));
}
