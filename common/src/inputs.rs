//! The ids of the controller inputs and of the controller profile in the messages to the game's
//! runtime in Wine (see `host/src/runtime_link.rs`). The runtime knows the ids of BasaltVR's
//! protocol: an id is the 64-bit hash of an OpenXR path, computed with Rust's `DefaultHasher` from
//! the path as a `str`. The constants below hold the results, so a change of the standard
//! library's hasher cannot change what the runtime reads. The test checks each constant against the
//! hash of its path with the hasher of the compiler that runs it.

/// The controller profile "/interaction_profiles/oculus/touch_controller"
pub const QUEST_TOUCH_PROFILE_ID: u64 = 0x0812_fd85_c843_84a6;

pub const LEFT_X_CLICK_ID: u64 = 0xb1a1_3c48_5685_e33c;
pub const RIGHT_X_CLICK_ID: u64 = 0x99e1_215a_0d42_6b4a;
pub const LEFT_X_TOUCH_ID: u64 = 0xc3d4_7643_2e89_d9dd;
pub const RIGHT_X_TOUCH_ID: u64 = 0x0b65_ff53_0a77_ac71;
pub const LEFT_Y_CLICK_ID: u64 = 0x5214_96d2_3e3e_9883;
pub const RIGHT_Y_CLICK_ID: u64 = 0xaafe_5312_f54d_d9d8;
pub const LEFT_Y_TOUCH_ID: u64 = 0x117d_0816_b380_e1f2;
pub const RIGHT_Y_TOUCH_ID: u64 = 0x7a18_cb6b_4088_7006;
pub const LEFT_A_CLICK_ID: u64 = 0x0665_55b8_9580_8431;
pub const RIGHT_A_CLICK_ID: u64 = 0xc72a_0ba6_ca52_fba7;
pub const LEFT_A_TOUCH_ID: u64 = 0xfd22_cec5_7d7e_2488;
pub const RIGHT_A_TOUCH_ID: u64 = 0x07ae_dc78_fdf1_bf2a;
pub const LEFT_B_CLICK_ID: u64 = 0x5c88_af3a_2dbf_9a1e;
pub const RIGHT_B_CLICK_ID: u64 = 0x9550_4746_6e34_669b;
pub const LEFT_B_TOUCH_ID: u64 = 0x5816_9224_6496_34fa;
pub const RIGHT_B_TOUCH_ID: u64 = 0xdd83_23ca_a5e4_8729;
pub const LEFT_MENU_CLICK_ID: u64 = 0x6268_a669_e87c_f910;
pub const RIGHT_MENU_CLICK_ID: u64 = 0xed68_cbf3_84f9_f033;
pub const LEFT_THUMBSTICK_CLICK_ID: u64 = 0xdfc8_a612_73cf_2569;
pub const RIGHT_THUMBSTICK_CLICK_ID: u64 = 0x00ac_9a92_f445_3d61;
pub const LEFT_THUMBSTICK_TOUCH_ID: u64 = 0x0966_df80_f498_9bfc;
pub const RIGHT_THUMBSTICK_TOUCH_ID: u64 = 0xef43_fe49_dfc8_7556;
pub const LEFT_THUMBSTICK_X_ID: u64 = 0xc3d7_09ed_0d79_4ea1;
pub const RIGHT_THUMBSTICK_X_ID: u64 = 0xd7c2_3648_f060_c6df;
pub const LEFT_THUMBSTICK_Y_ID: u64 = 0xcb7d_9287_0626_3554;
pub const RIGHT_THUMBSTICK_Y_ID: u64 = 0xcee8_c338_e02e_0131;
pub const LEFT_TRIGGER_CLICK_ID: u64 = 0x4348_1636_889d_fed0;
pub const RIGHT_TRIGGER_CLICK_ID: u64 = 0xc6aa_9987_f4cc_c0a7;
pub const LEFT_TRIGGER_TOUCH_ID: u64 = 0x4fa5_7f59_dbae_033a;
pub const RIGHT_TRIGGER_TOUCH_ID: u64 = 0xe0a7_e848_dde3_40b0;
pub const LEFT_TRIGGER_VALUE_ID: u64 = 0x0b66_bbe3_dc2d_c49f;
pub const RIGHT_TRIGGER_VALUE_ID: u64 = 0xede7_e982_d5ee_df21;
pub const LEFT_SQUEEZE_CLICK_ID: u64 = 0x8b49_61b6_06fc_a1bd;
pub const RIGHT_SQUEEZE_CLICK_ID: u64 = 0x3dd6_a3ba_ec0c_e32c;
pub const LEFT_SQUEEZE_VALUE_ID: u64 = 0x01f1_b9c6_8980_4b82;
pub const RIGHT_SQUEEZE_VALUE_ID: u64 = 0xdd59_35cc_0051_36f1;
pub const LEFT_THUMBREST_TOUCH_ID: u64 = 0x3ce1_c9ed_4d12_f3c2;
pub const RIGHT_THUMBREST_TOUCH_ID: u64 = 0x947e_5a8f_8780_c92d;

/// Every input id with its OpenXR path, for the test
#[cfg(test)]
const PATHS: [(u64, &str); 39] = [
    (QUEST_TOUCH_PROFILE_ID, "/interaction_profiles/oculus/touch_controller"),
    (LEFT_X_CLICK_ID, "/user/hand/left/input/x/click"),
    (RIGHT_X_CLICK_ID, "/user/hand/right/input/x/click"),
    (LEFT_X_TOUCH_ID, "/user/hand/left/input/x/touch"),
    (RIGHT_X_TOUCH_ID, "/user/hand/right/input/x/touch"),
    (LEFT_Y_CLICK_ID, "/user/hand/left/input/y/click"),
    (RIGHT_Y_CLICK_ID, "/user/hand/right/input/y/click"),
    (LEFT_Y_TOUCH_ID, "/user/hand/left/input/y/touch"),
    (RIGHT_Y_TOUCH_ID, "/user/hand/right/input/y/touch"),
    (LEFT_A_CLICK_ID, "/user/hand/left/input/a/click"),
    (RIGHT_A_CLICK_ID, "/user/hand/right/input/a/click"),
    (LEFT_A_TOUCH_ID, "/user/hand/left/input/a/touch"),
    (RIGHT_A_TOUCH_ID, "/user/hand/right/input/a/touch"),
    (LEFT_B_CLICK_ID, "/user/hand/left/input/b/click"),
    (RIGHT_B_CLICK_ID, "/user/hand/right/input/b/click"),
    (LEFT_B_TOUCH_ID, "/user/hand/left/input/b/touch"),
    (RIGHT_B_TOUCH_ID, "/user/hand/right/input/b/touch"),
    (LEFT_MENU_CLICK_ID, "/user/hand/left/input/menu/click"),
    (RIGHT_MENU_CLICK_ID, "/user/hand/right/input/menu/click"),
    (LEFT_THUMBSTICK_CLICK_ID, "/user/hand/left/input/thumbstick/click"),
    (RIGHT_THUMBSTICK_CLICK_ID, "/user/hand/right/input/thumbstick/click"),
    (LEFT_THUMBSTICK_TOUCH_ID, "/user/hand/left/input/thumbstick/touch"),
    (RIGHT_THUMBSTICK_TOUCH_ID, "/user/hand/right/input/thumbstick/touch"),
    (LEFT_THUMBSTICK_X_ID, "/user/hand/left/input/thumbstick/x"),
    (RIGHT_THUMBSTICK_X_ID, "/user/hand/right/input/thumbstick/x"),
    (LEFT_THUMBSTICK_Y_ID, "/user/hand/left/input/thumbstick/y"),
    (RIGHT_THUMBSTICK_Y_ID, "/user/hand/right/input/thumbstick/y"),
    (LEFT_TRIGGER_CLICK_ID, "/user/hand/left/input/trigger/click"),
    (RIGHT_TRIGGER_CLICK_ID, "/user/hand/right/input/trigger/click"),
    (LEFT_TRIGGER_TOUCH_ID, "/user/hand/left/input/trigger/touch"),
    (RIGHT_TRIGGER_TOUCH_ID, "/user/hand/right/input/trigger/touch"),
    (LEFT_TRIGGER_VALUE_ID, "/user/hand/left/input/trigger/value"),
    (RIGHT_TRIGGER_VALUE_ID, "/user/hand/right/input/trigger/value"),
    (LEFT_SQUEEZE_CLICK_ID, "/user/hand/left/input/squeeze/click"),
    (RIGHT_SQUEEZE_CLICK_ID, "/user/hand/right/input/squeeze/click"),
    (LEFT_SQUEEZE_VALUE_ID, "/user/hand/left/input/squeeze/value"),
    (RIGHT_SQUEEZE_VALUE_ID, "/user/hand/right/input/squeeze/value"),
    (LEFT_THUMBREST_TOUCH_ID, "/user/hand/left/input/thumbrest/touch"),
    (RIGHT_THUMBREST_TOUCH_ID, "/user/hand/right/input/thumbrest/touch"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
    };

    #[test]
    fn each_id_is_the_hash_of_its_path() {
        for (id, path) in PATHS {
            let mut hasher = DefaultHasher::new();
            path.hash(&mut hasher);
            assert_eq!(id, hasher.finish(), "{path}");
        }
    }

    #[test]
    fn no_two_inputs_share_an_id() {
        let mut ids: Vec<u64> = PATHS.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PATHS.len());
    }
}
