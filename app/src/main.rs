//! OpenAirCare: a Makepad desktop UI over `airpods-link`.
//!
//! UI -> session: `SessionHandle::send(Command)`.
//! Session -> UI: the session thread posts `SessionEvent`s with
//! `Cx::post_action`; `handle_actions` downcasts them and refreshes widgets.

pub use makepad_widgets;

mod hearing_test;
mod settings;
mod setup;
mod tone;

use std::sync::Arc;

use airpods_link::session::{Command, ConnectionState, SessionEvent, SessionHandle, SetupEvent, Snapshot};
use airpods_link::{DeviceEntry, HostStatus, Transport};
use airpods_proto::hearing::{Adjustments, Audiogram, BANDS_HZ, DB_HL_MAX, DB_HL_MIN};
use airpods_proto::model;
use airpods_proto::ListeningMode;
use makepad_widgets::*;

use crate::hearing_test::{Presentation, TestRunner};
use crate::settings::Settings;
use crate::setup::{SetupAction, SetupDone};
use crate::tone::{db_hl_to_amp, Ear, ToneShared, BURST_SECS};

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*

    // The theme draws text as white at ~35% alpha (`color_u_5`), which reads as
    // grey on the dark background above. Widget prototypes bake their colour in
    // when the prelude is built, so overriding `theme.color_text` afterwards has
    // no effect -- restyle the types this UI actually uses instead.
    // `color_disabled` and `color_empty` (placeholder) are left dimmer on purpose,
    // so greyed-out controls and empty fields still read as such.
    let Label = Label{draw_text +: {color: #FFFFFFFF}}
    let H3 = H3{draw_text +: {color: #FFFFFFFF}}
    let Button = Button{draw_text +: {
        color: #FFFFFFFF color_hover: #FFFFFFFF color_down: #FFFFFFFF color_focus: #FFFFFFFF
    }}
    let TextInput = TextInput{draw_text +: {
        color: #FFFFFFFF color_hover: #FFFFFFFF color_focus: #FFFFFFFF color_down: #FFFFFFFF
    }}
    let CheckBox = CheckBox{draw_text +: {
        color: #FFFFFFFF color_hover: #FFFFFFFF color_down: #FFFFFFFF
        color_focus: #FFFFFFFF color_active: #FFFFFFFF
    }}
    let Toggle = Toggle{draw_text +: {
        color: #FFFFFFFF color_hover: #FFFFFFFF color_down: #FFFFFFFF
        color_focus: #FFFFFFFF color_active: #FFFFFFFF
    }}
    let RadioButtonTab = RadioButtonTab{draw_text +: {
        color: #FFFFFFFF color_hover: #FFFFFFFF color_down: #FFFFFFFF
        color_focus: #FFFFFFFF color_active: #FFFFFFFF
    }}
    // The slider's own label plus the editable value field inside it.
    let Slider = Slider{
        draw_text +: {
            color: #FFFFFFFF color_hover: #FFFFFFFF color_drag: #FFFFFFFF color_focus: #FFFFFFFF
        }
        text_input +: {draw_text +: {
            color: #FFFFFFFF color_hover: #FFFFFFFF color_focus: #FFFFFFFF color_down: #FFFFFFFF
        }}
    }

    let app = startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "OpenAirCare"
                window.inner_size: vec2(600, 820)
                // Darker than the theme's default `color_bg_app` (a mid grey).
                pass +: { clear_color: #141414 }
                body +: {
                    flow: Overlay
                    View{
                        width: Fill height: Fill
                        flow: Down
                        padding: 12
                        spacing: 10

                        tabs := View{
                            width: Fit height: Fit
                            flow: Right
                            spacing: theme.space_2
                            tab_setup := RadioButtonTab{text: "Setup"}
                            tab_status := RadioButtonTab{text: "Status"}
                            tab_hearing := RadioButtonTab{text: "Hearing Health"}
                            tab_test := RadioButtonTab{text: "Hearing Test"}
                            tab_audiogram := RadioButtonTab{text: "Audiogram"}
                            tab_adjust := RadioButtonTab{text: "Adjustments"}
                        }

                        pages := PageFlip{
                            width: Fill height: Fill
                            flow: Down
                            active_page: @page_status

                            // ---------------------------------------------------- Setup
                            page_setup := ScrollYView{
                                width: Fill height: Fill
                                flow: Down
                                spacing: 8

                                H3{text: "1. Bluetooth"}
                                setup_bt_label := Label{width: Fill text: "Checking Bluetooth..."}
                                Label{
                                    width: Fill
                                    text: "Set up Bluetooth asks for your password, then restarts Bluetooth: mice, keyboards and headphones on Bluetooth drop for a few seconds."
                                }
                                setup_apply_btn := Button{text: "Set up Bluetooth"}

                                Hr{}
                                H3{text: "2. AirPods"}
                                setup_pair_label := Label{width: Fill text: ""}
                                Label{
                                    width: Fill
                                    text: "To pair: put the AirPods in the case, leave the lid open, and hold the button on the back of the case until the light flashes white. Then search."
                                }
                                View{
                                    width: Fill height: Fit
                                    flow: Right
                                    spacing: 8
                                    setup_scan_btn := Button{text: "Search for AirPods"}
                                    setup_forget_btn := Button{text: "Remove pairing"}
                                }
                                setup_found_0 := Button{visible: false text: ""}
                                setup_found_1 := Button{visible: false text: ""}
                                setup_found_2 := Button{visible: false text: ""}
                                setup_pair_status := Label{width: Fill text: ""}

                                Hr{}
                                H3{text: "3. Connect"}
                                Label{width: Fill text: "Wear the AirPods, open the Status tab and press Connect."}

                                Hr{}
                                View{
                                    width: Fill height: Fit
                                    flow: Right
                                    spacing: 8
                                    setup_check_btn := Button{text: "Check again"}
                                    setup_menu_btn := Button{text: "Add to app menu"}
                                    setup_undo_btn := Button{text: "Undo Bluetooth changes"}
                                }
                                setup_msg_label := Label{width: Fill text: ""}
                            }

                            // ---------------------------------------------------- Status
                            page_status := ScrollYView{
                                width: Fill height: Fill
                                flow: Down
                                spacing: 8

                                H3{text: "Connection"}
                                state_label := Label{width: Fill text: "Disconnected"}
                                error_label := Label{width: Fill text: ""}
                                device_label := Label{width: Fill text: "No device"}
                                battery_label := Label{width: Fill text: ""}
                                View{
                                    width: Fill height: Fit
                                    flow: Right
                                    spacing: 8
                                    connect_btn := Button{text: "Connect"}
                                    disconnect_btn := Button{text: "Disconnect"}
                                    refresh_btn := Button{text: "List devices"}
                                }
                                Label{text: "Device MAC (leave empty for the first paired AirPods)"}
                                mac_input := TextInput{
                                    width: Fill height: 36
                                    empty_text: "XX:XX:XX:XX:XX:XX"
                                    autocorrect: Disabled
                                    autocapitalize: None
                                }
                                auto_reconnect_check := CheckBox{text: "Reconnect automatically", active: true}

                                Hr{}
                                H3{text: "Listening mode"}
                                Label{width: Fill text: "Hearing Health only works in Transparency mode."}
                                modes := View{
                                    width: Fit height: Fit
                                    flow: Right
                                    spacing: theme.space_2
                                    mode_off := RadioButtonTab{text: "Off"}
                                    mode_anc := RadioButtonTab{text: "Noise Cancellation"}
                                    mode_transparency := RadioButtonTab{text: "Transparency"}
                                    mode_adaptive := RadioButtonTab{text: "Adaptive"}
                                }

                                Hr{}
                                H3{text: "Log"}
                                log_label := Label{width: Fill text: ""}
                            }

                            // ---------------------------------------------------- Hearing health
                            page_hearing := ScrollYView{
                                width: Fill height: Fill
                                flow: Down
                                spacing: 10

                                H3{text: "Hearing Health"}
                                capability_label := Label{width: Fill text: "Connect your AirPods first."}
                                ha_toggle := Toggle{text: "Hearing Health enabled"}
                                swipe_toggle := Toggle{text: "Swipe the stem to adjust amplification"}
                                Hr{}
                                att_label := Label{width: Fill text: ""}
                                Label{
                                    width: Fill
                                    text: "Enabling Hearing Health turns off Customized Transparency and Headphone Accommodation on the AirPods. Use the Audiogram tab to load your hearing test results, then fine tune on the Adjustments tab."
                                }
                            }

                            // ---------------------------------------------------- Hearing test
                            page_test := ScrollYView{
                                width: Fill height: Fill
                                flow: Down
                                spacing: 10

                                H3{text: "Hearing Test (pure-tone screening)"}
                                Label{
                                    width: Fill
                                    text: "Plays short beeps at the 8 audiogram bands, one ear at a time, and finds the quietest level you respond to. This is a screening aid, not a clinical test: tone levels are estimated, not calibrated. The results fill the Audiogram tab for you to review before applying."
                                }
                                Label{
                                    width: Fill
                                    text: "Before you start: sit somewhere quiet, wear both AirPods, set the system volume to 100 %, and make sure the AirPods are the audio output. Remove the AirPods at once if anything is uncomfortably loud."
                                }
                                ht_device_label := Label{width: Fill text: "Audio output: waiting for the device list..."}
                                ht_buds_label := Label{width: Fill text: ""}
                                ht_offset_slider := Slider{text: "Level offset (dB)  - the sample tone should be clearly audible but not loud; adjust if it is not" min: -20.0 max: 20.0 step: 1.0 precision: 0 default: 0.0}
                                ht_manual_toggle := Toggle{text: "Manual mode: set each tone's level yourself"}
                                ht_auto_view := View{
                                    width: Fill height: Fit flow: Down spacing: 10
                                    View{
                                        width: Fill height: Fit flow: Right spacing: 8
                                        ht_sample_btn := Button{text: "Play sample tone (1 kHz, 40 dB HL)"}
                                        ht_start_btn := Button{text: "Start test"}
                                        ht_stop_btn := Button{text: "Stop"}
                                    }
                                    Hr{}
                                    ht_progress_label := Label{width: Fill text: ""}
                                    ht_state_label := Label{width: Fill text: "Not running."}
                                    ht_heard_btn := Button{width: Fill height: 90 text: "I heard it   (or press Space)"}
                                    ht_false_label := Label{width: Fill text: "" draw_text +: {color: #FFB020FF}}
                                    Hr{}
                                    ht_results_label := Label{width: Fill text: ""}
                                    ht_use_btn := Button{text: "Use in Audiogram tab"}
                                }
                                ht_manual_view := View{
                                    visible: false
                                    width: Fill height: Fit flow: Down spacing: 10
                                    Label{
                                        width: Fill
                                        text: "Drag a slider from the left: that band's beeps play in that ear while you hold it, and get louder as you move right. Leave each slider where the beeps are only just audible. Levels are estimated, not calibrated, and are capped like the automatic test. Remove the AirPods at once if anything is uncomfortably loud."
                                    }
                                    View{
                                        width: Fill height: Fit flow: Right spacing: 10
                                        View{
                                            width: Fill height: Fit flow: Down spacing: 4
                                            H3{text: "Left ear"}
                                            ht_m_l_0 := Slider{width: Fill text: "250 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_l_1 := Slider{width: Fill text: "500 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_l_2 := Slider{width: Fill text: "1000 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_l_3 := Slider{width: Fill text: "2000 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_l_4 := Slider{width: Fill text: "3000 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_l_5 := Slider{width: Fill text: "4000 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_l_6 := Slider{width: Fill text: "6000 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_l_7 := Slider{width: Fill text: "8000 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                        }
                                        View{
                                            width: Fill height: Fit flow: Down spacing: 4
                                            H3{text: "Right ear"}
                                            ht_m_r_0 := Slider{width: Fill text: "250 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_r_1 := Slider{width: Fill text: "500 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_r_2 := Slider{width: Fill text: "1000 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_r_3 := Slider{width: Fill text: "2000 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_r_4 := Slider{width: Fill text: "3000 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_r_5 := Slider{width: Fill text: "4000 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_r_6 := Slider{width: Fill text: "6000 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                            ht_m_r_7 := Slider{width: Fill text: "8000 Hz" min: -10.0 max: 80.0 step: 5.0 precision: 0 default: 0.0}
                                        }
                                    }
                                    ht_manual_use_btn := Button{text: "Use in Audiogram tab"}
                                }
                            }

                            // ---------------------------------------------------- Audiogram
                            page_audiogram := ScrollYView{
                                width: Fill height: Fill
                                flow: Down
                                spacing: 8

                                H3{text: "Audiogram (hearing loss in dB HL)"}
                                Label{width: Fill text: "Enter the values from a professional hearing test. 0 = no loss, 120 = maximum. AirPods use bands 250 Hz to 8 kHz."}
                                View{
                                    width: Fill height: Fit flow: Right spacing: 8
                                    Label{width: 80 text: "Band"}
                                    Label{width: 100 text: "Left"}
                                    Label{width: 100 text: "Right"}
                                }
                                View{ width: Fill height: Fit flow: Right spacing: 8
                                    Label{width: 80 text: "250 Hz"}
                                    ag_l_0 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                    ag_r_0 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                }
                                View{ width: Fill height: Fit flow: Right spacing: 8
                                    Label{width: 80 text: "500 Hz"}
                                    ag_l_1 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                    ag_r_1 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                }
                                View{ width: Fill height: Fit flow: Right spacing: 8
                                    Label{width: 80 text: "1 kHz"}
                                    ag_l_2 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                    ag_r_2 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                }
                                View{ width: Fill height: Fit flow: Right spacing: 8
                                    Label{width: 80 text: "2 kHz"}
                                    ag_l_3 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                    ag_r_3 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                }
                                View{ width: Fill height: Fit flow: Right spacing: 8
                                    Label{width: 80 text: "3 kHz"}
                                    ag_l_4 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                    ag_r_4 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                }
                                View{ width: Fill height: Fit flow: Right spacing: 8
                                    Label{width: 80 text: "4 kHz"}
                                    ag_l_5 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                    ag_r_5 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                }
                                View{ width: Fill height: Fit flow: Right spacing: 8
                                    Label{width: 80 text: "6 kHz"}
                                    ag_l_6 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                    ag_r_6 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                }
                                View{ width: Fill height: Fit flow: Right spacing: 8
                                    Label{width: 80 text: "8 kHz"}
                                    ag_l_7 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                    ag_r_7 := TextInput{width: 100 height: 32 is_numeric_only: true empty_text: "0"}
                                }
                                View{
                                    width: Fill height: Fit flow: Right spacing: 8
                                    ag_apply_btn := Button{text: "Apply to AirPods"}
                                    ag_reload_btn := Button{text: "Reload from AirPods"}
                                }
                                View{
                                    width: Fill height: Fit flow: Right spacing: 8
                                    ag_save_btn := Button{text: "Save locally"}
                                    ag_load_btn := Button{text: "Load saved"}
                                }
                                ag_status_label := Label{width: Fill text: ""}
                            }

                            // ---------------------------------------------------- Adjustments
                            page_adjust := ScrollYView{
                                width: Fill height: Fill
                                flow: Down
                                spacing: 12

                                H3{text: "Adjustments"}
                                adj_hint_label := Label{width: Fill text: ""}
                                amp_slider := Slider{text: "Amplification  (1.00 = Apple's maximum; above that asks for confirmation, hard limit 1.50)" min: -1.0 max: 1.5 step: 0.01 precision: 2 default: 0.0}
                                amp_warn_label := Label{width: Fill text: "" draw_text +: {color: #FFB020FF}}
                                bal_slider := Slider{text: "Balance  (left  <->  right)" min: -1.0 max: 1.0 step: 0.01 precision: 2 default: 0.0}
                                tone_slider := Slider{text: "Tone  (darker  <->  brighter)" min: -1.0 max: 1.0 step: 0.01 precision: 2 default: 0.0}
                                anr_slider := Slider{text: "Ambient noise reduction" min: 0.0 max: 1.0 step: 0.01 precision: 2 default: 0.0}
                                conv_check := CheckBox{text: "Conversation boost"}
                                own_voice_slider := Slider{text: "Own voice amplification  (how loud you hear yourself)" min: 0.0 max: 1.0 step: 0.01 precision: 2 default: 0.0}
                                reset_btn := Button{text: "Reset adjustments"}
                            }
                        }
                    }

                    // Shown when the amplification slider is released above
                    // Apple's maximum. Only the two buttons close it.
                    amp_warn_modal := Modal{
                        can_dismiss: false
                        content +: {
                            width: 460
                            height: Fit
                            RoundedView{
                                width: Fill height: Fit
                                show_bg: true
                                draw_bg.color: #3A2A10
                                draw_bg.border_color: #FFB020
                                draw_bg.border_size: 1.0
                                draw_bg.border_radius: 8.0
                                padding: 22 spacing: 12
                                flow: Down
                                H3{text: "Warning: amplification above safe limits"}
                                amp_warn_value_label := Label{width: Fill text: ""}
                                Label{
                                    width: Fill
                                    text: "Apple's own controls stop at 1.00. Values above that are outside the range Apple allows and beyond safe listening limits. Boosting this high could damage your hearing and/or the AirPods."
                                }
                                Label{width: Fill text: "Are you sure you want to boost above Apple's maximum?"}
                                View{
                                    width: Fill height: Fit
                                    flow: Right spacing: 10 align: Align{x: 1.0 y: 0.5}
                                    amp_boost_cancel_btn := Button{text: "No, keep at 1.00"}
                                    amp_boost_confirm_btn := Button{text: "Yes, boost above 1.00"}
                                }
                            }
                        }
                    }


                    // Shown when the user presses Start on the Hearing Test tab.
                    ht_warn_modal := Modal{
                        can_dismiss: false
                        content +: {
                            width: 460
                            height: Fit
                            RoundedView{
                                width: Fill height: Fit
                                show_bg: true
                                draw_bg.color: #3A2A10
                                draw_bg.border_color: #FFB020
                                draw_bg.border_size: 1.0
                                draw_bg.border_radius: 8.0
                                padding: 22 spacing: 12
                                flow: Down
                                H3{text: "Before the hearing test"}
                                Label{
                                    width: Fill
                                    text: "Tones start at a moderate level and get louder only when you do not respond, up to a fixed cap. The levels are estimated from typical AirPods Pro output and are not calibrated, so the result is a screening aid, not a diagnosis."
                                }
                                Label{
                                    width: Fill
                                    text: "Remove the AirPods immediately if any tone is uncomfortably loud, and see a hearing-care professional for a real audiogram."
                                }
                                View{
                                    width: Fill height: Fit
                                    flow: Right spacing: 10 align: Align{x: 1.0 y: 0.5}
                                    ht_warn_cancel_btn := Button{text: "Cancel"}
                                    ht_warn_start_btn := Button{text: "Start test"}
                                }
                            }
                        }
                    }

                    // First-launch disclaimer. Opened at startup until the
                    // user has acknowledged it once; only the two buttons
                    // close it, and the session stays disconnected until
                    // the user accepts.
                    disclaimer_modal := Modal{
                        can_dismiss: false
                        content +: {
                            width: 520
                            height: Fit
                            RoundedView{
                                width: Fill height: Fit
                                show_bg: true
                                draw_bg.color: #3A1010
                                draw_bg.border_color: #FF5050
                                draw_bg.border_size: 1.0
                                draw_bg.border_radius: 8.0
                                padding: 22 spacing: 12
                                flow: Down
                                H3{text: "Read before first use"}
                                Label{
                                    width: Fill
                                    text: "This software is an uncertified, experimental research utility for Linux hardware interoperability. It is NOT a medical device, is NOT approved by any regulatory health agency (including the FDA or Health Canada), and must NOT be used as a replacement for a prescribed hearing aid. The developers accept no liability for hearing damage or device malfunction."
                                }
                                Label{
                                    width: Fill
                                    text: "If you have, or suspect, hearing loss, see a qualified hearing-care professional. Stop and remove the AirPods immediately if you notice discomfort, ringing, or unexpectedly loud output."
                                }
                                Label{
                                    width: Fill
                                    text: "AirPods and AirPods Pro are registered trademarks of Apple Inc. This project is independent and not affiliated with, endorsed by, or sponsored by Apple Inc."
                                }
                                View{
                                    width: Fill height: Fit
                                    flow: Right spacing: 10 align: Align{x: 1.0 y: 0.5}
                                    disclaimer_decline_btn := Button{text: "Quit"}
                                    disclaimer_accept_btn := Button{text: "I have read the above and accept"}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    app
}

/// Apple's own UI stops here. Anything above is written to the buds only
/// after the user confirms the warning dialog.
const SAFE_AMP_MAX: f32 = 1.0;

const MAX_LOG_LINES: usize = 14;

/// Hearing test timing (seconds). The silence before a burst is random in
/// `PRE_DELAY_MIN..PRE_DELAY_MIN + PRE_DELAY_SPREAD` so the user cannot
/// anticipate it; a response is accepted during the burst and for
/// `RESPONSE_WINDOW` after it.
const PRE_DELAY_MIN: f64 = 1.0;
const PRE_DELAY_SPREAD: f64 = 1.5;
const RESPONSE_WINDOW: f64 = 1.5;
/// Presses during the silence before a tone; above this the user is warned.
const FALSE_PRESS_WARN: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TestPhase {
    #[default]
    Idle,
    /// Silence before the burst.
    PreDelay,
    /// The burst is playing.
    Playing,
    /// Burst over, still accepting a response.
    Window,
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    session: Option<SessionHandle>,
    #[rust]
    snap: Option<Snapshot>,
    #[rust]
    logs: Vec<String>,
    #[rust]
    settings: Settings,
    /// The user edited audiogram fields since the last device sync.
    #[rust]
    audiogram_dirty: bool,
    /// The user confirmed the warning and may boost above [`SAFE_AMP_MAX`].
    /// Re-armed once the amplification drops back to the safe range.
    #[rust]
    amp_boost_confirmed: bool,
    /// The slider sits above [`SAFE_AMP_MAX`] without confirmation: the buds
    /// are held at the safe value and the slider is not overwritten from the
    /// device snapshot until the user answers the dialog.
    #[rust]
    amp_boost_pending: bool,
    /// Handle to the tone generator on the audio thread.
    #[rust]
    tone: ToneShared,
    /// Hearing test in progress, if any.
    #[rust]
    test: Option<TestRunner>,
    #[rust]
    test_phase: TestPhase,
    #[rust]
    test_timer: Timer,
    /// Presses while no tone was playing, this run.
    #[rust]
    false_presses: u32,
    /// Finished run waiting to be copied into the Audiogram tab.
    #[rust]
    test_result: Option<(Audiogram, Vec<String>)>,
    /// Listening mode and Hearing Health state to put back when the
    /// automatic test or manual mode ends.
    #[rust]
    buds_restore: Option<(Option<ListeningMode>, Option<bool>)>,
    /// The manual (slider) sub-page is shown instead of the automatic test.
    #[rust]
    manual_mode: bool,
    /// Manual-mode slider currently held down, so its tone is playing.
    #[rust]
    manual_playing: Option<(Ear, usize)>,
    /// xorshift state for the pre-tone delay (no `rand` dependency).
    #[rust]
    rng: u64,
    /// Latest answer to `Command::CheckSetup`.
    #[rust]
    host: Option<HostStatus>,
    /// The Setup tab was offered (or found unnecessary) once this launch.
    #[rust]
    setup_tab_decided: bool,
    /// A password-prompt action (`setup::spawn`) is running.
    #[rust]
    setup_busy: bool,
    #[rust]
    scanning: bool,
    #[rust]
    pairing: bool,
    /// Scan results behind the `setup_found_*` buttons.
    #[rust]
    found: Vec<DeviceEntry>,
    /// The Apple identity was just set while AirPods were already paired:
    /// they still hold the old identity and need pairing again.
    #[rust]
    repair_hint: bool,
}

/// Tab buttons with their pages, in display order.
fn tabs() -> [(&'static [LiveId], LiveId); 6] {
    [
        (ids!(tab_setup), live_id!(page_setup)),
        (ids!(tab_status), live_id!(page_status)),
        (ids!(tab_hearing), live_id!(page_hearing)),
        (ids!(tab_test), live_id!(page_test)),
        (ids!(tab_audiogram), live_id!(page_audiogram)),
        (ids!(tab_adjust), live_id!(page_adjust)),
    ]
}

fn found_ids() -> [&'static [LiveId]; 3] {
    [ids!(setup_found_0), ids!(setup_found_1), ids!(setup_found_2)]
}

fn left_ids() -> [&'static [LiveId]; 8] {
    [
        ids!(ag_l_0), ids!(ag_l_1), ids!(ag_l_2), ids!(ag_l_3),
        ids!(ag_l_4), ids!(ag_l_5), ids!(ag_l_6), ids!(ag_l_7),
    ]
}

fn right_ids() -> [&'static [LiveId]; 8] {
    [
        ids!(ag_r_0), ids!(ag_r_1), ids!(ag_r_2), ids!(ag_r_3),
        ids!(ag_r_4), ids!(ag_r_5), ids!(ag_r_6), ids!(ag_r_7),
    ]
}

/// Manual-mode sliders, one per audiogram band.
fn manual_left_ids() -> [&'static [LiveId]; 8] {
    [
        ids!(ht_m_l_0), ids!(ht_m_l_1), ids!(ht_m_l_2), ids!(ht_m_l_3),
        ids!(ht_m_l_4), ids!(ht_m_l_5), ids!(ht_m_l_6), ids!(ht_m_l_7),
    ]
}

fn manual_right_ids() -> [&'static [LiveId]; 8] {
    [
        ids!(ht_m_r_0), ids!(ht_m_r_1), ids!(ht_m_r_2), ids!(ht_m_r_3),
        ids!(ht_m_r_4), ids!(ht_m_r_5), ids!(ht_m_r_6), ids!(ht_m_r_7),
    ]
}

fn make_transport() -> Arc<dyn Transport> {
    #[cfg(feature = "mock")]
    {
        ::log::info!("using the mock AirPods transport");
        let mock = airpods_link::mock::MockTransport::new();
        // Start like a computer that was never set up, to exercise the Setup tab.
        if std::env::var_os("OPENAIRCARE_MOCK_FRESH").is_some() {
            let mut st = mock.state.lock().unwrap();
            st.paired = false;
            st.apple_identity = false;
        }
        return Arc::new(mock);
    }
    #[allow(unreachable_code)]
    airpods_link::default_transport()
        .expect("no Bluetooth transport on this platform; build with `--features mock` for development")
}

impl App {
    fn send(&self, cmd: Command) {
        if let Some(s) = &self.session {
            s.send(cmd);
        }
    }

    fn push_log(&mut self, cx: &mut Cx, line: String) {
        self.logs.push(line);
        if self.logs.len() > MAX_LOG_LINES {
            let drop = self.logs.len() - MAX_LOG_LINES;
            self.logs.drain(..drop);
        }
        let text = self.logs.join("\n");
        self.ui.label(cx, ids!(log_label)).set_text(cx, &text);
    }

    fn mac_from_input(&self, cx: &mut Cx) -> Option<String> {
        let t = self.ui.text_input(cx, ids!(mac_input)).text();
        let t = t.trim().to_uppercase();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    }

    fn adjustments_from_ui(&self, cx: &mut Cx) -> Adjustments {
        let v = |id: &[LiveId]| self.ui.slider(cx, id).value().unwrap_or(0.0) as f32;
        Adjustments {
            amplification: v(ids!(amp_slider)),
            balance: v(ids!(bal_slider)),
            tone: v(ids!(tone_slider)),
            ambient_noise_reduction: v(ids!(anr_slider)),
            conversation_boost: self.ui.check_box(cx, ids!(conv_check)).active(cx),
        }
    }

    fn audiogram_from_ui(&self, cx: &mut Cx) -> Result<Audiogram, String> {
        let mut ag = Audiogram::default();
        for (i, (l, r)) in left_ids().iter().zip(right_ids().iter()).enumerate() {
            ag.left[i] = parse_db(&self.ui.text_input(cx, l).text()).map_err(|e| format!("left {} Hz: {e}", BANDS_HZ[i]))?;
            ag.right[i] = parse_db(&self.ui.text_input(cx, r).text()).map_err(|e| format!("right {} Hz: {e}", BANDS_HZ[i]))?;
        }
        Ok(ag)
    }

    fn audiogram_to_ui(&mut self, cx: &mut Cx, ag: &Audiogram) {
        for (i, (l, r)) in left_ids().iter().zip(right_ids().iter()).enumerate() {
            self.ui.text_input(cx, l).set_text(cx, &format_db(ag.left[i]));
            self.ui.text_input(cx, r).set_text(cx, &format_db(ag.right[i]));
        }
        self.audiogram_dirty = false;
    }

    fn adjustments_to_ui(&mut self, cx: &mut Cx, a: &Adjustments) {
        if !self.amp_boost_pending {
            self.ui.slider(cx, ids!(amp_slider)).set_value(cx, a.amplification as f64);
        }
        self.ui.slider(cx, ids!(bal_slider)).set_value(cx, a.balance as f64);
        self.ui.slider(cx, ids!(tone_slider)).set_value(cx, a.tone as f64);
        self.ui.slider(cx, ids!(anr_slider)).set_value(cx, a.ambient_noise_reduction as f64);
        self.ui.check_box(cx, ids!(conv_check)).set_active(cx, a.conversation_boost, Animate::No);
        self.update_amp_warning(cx);
    }

    fn amp_slider_value(&self, cx: &mut Cx) -> f32 {
        self.ui.slider(cx, ids!(amp_slider)).value().unwrap_or(0.0) as f32
    }

    /// Inline warning under the amplification slider whenever it is above
    /// Apple's maximum (pending or confirmed).
    fn update_amp_warning(&mut self, cx: &mut Cx) {
        let amp = self.amp_slider_value(cx);
        let text = if amp <= SAFE_AMP_MAX {
            String::new()
        } else if self.amp_boost_pending {
            format!(
                "Warning: {amp:.2} is above Apple's maximum of {SAFE_AMP_MAX:.2}. The AirPods are held at {SAFE_AMP_MAX:.2} until you confirm.",
            )
        } else {
            format!(
                "Warning: {amp:.2} is above Apple's maximum of {SAFE_AMP_MAX:.2} and above safe listening limits. This could damage your hearing and/or the AirPods.",
            )
        };
        self.ui.label(cx, ids!(amp_warn_label)).set_text(cx, &text);
    }

    /// Read the adjustments from the UI and write them to the buds, holding
    /// the amplification at [`SAFE_AMP_MAX`] until a higher value is confirmed.
    fn apply_adjustments_from_ui(&mut self, cx: &mut Cx) {
        let mut adj = self.adjustments_from_ui(cx);
        if adj.amplification > SAFE_AMP_MAX {
            if !self.amp_boost_confirmed {
                self.amp_boost_pending = true;
                adj.amplification = SAFE_AMP_MAX;
            }
        } else {
            // Back in the safe range: ask again the next time the user goes above it.
            self.amp_boost_pending = false;
            self.amp_boost_confirmed = false;
        }
        self.settings.adjustments = Some(adj);
        self.send(Command::SetAdjustments(adj));
        self.update_amp_warning(cx);
    }

    // ---- hearing test ----

    fn rand_unit(&mut self) -> f64 {
        if self.rng == 0 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E37_79B9_7F4A_7C15);
            self.rng = nanos | 1;
        }
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    fn test_offset_db(&self, cx: &mut Cx) -> f32 {
        self.ui.slider(cx, ids!(ht_offset_slider)).value().unwrap_or(0.0) as f32
    }

    fn test_running(&self) -> bool {
        self.test.is_some()
    }

    /// Isolate the tones: ANC on, Hearing Health off. The previous state is
    /// kept for `restore_buds`; a second call while isolated is a no-op so
    /// the isolated state is never mistaken for the original.
    fn isolate_buds(&mut self) {
        if self.buds_restore.is_some() {
            return;
        }
        if let Some(snap) = &self.snap {
            if matches!(snap.state, ConnectionState::Connected) {
                self.buds_restore = Some((snap.listening_mode, snap.hearing_aid_enabled));
                self.send(Command::SetListeningMode(ListeningMode::NoiseCancellation));
                if snap.hearing_aid_enabled == Some(true) {
                    self.send(Command::SetHearingAid(false));
                }
            }
        }
    }

    fn restore_buds(&mut self, cx: &mut Cx) {
        if let Some((mode, ha)) = self.buds_restore.take() {
            if let Some(m) = mode {
                self.send(Command::SetListeningMode(m));
            }
            if ha == Some(true) {
                self.send(Command::SetHearingAid(true));
            }
            self.push_log(cx, "hearing test: restored listening mode / Hearing Health".into());
        }
    }

    fn begin_test(&mut self, cx: &mut Cx) {
        if self.test_running() || self.manual_mode {
            return;
        }
        self.isolate_buds();
        self.test = Some(TestRunner::new());
        self.false_presses = 0;
        self.test_result = None;
        self.ui.label(cx, ids!(ht_results_label)).set_text(cx, "");
        self.push_log(cx, "hearing test started".into());
        self.next_trial(cx);
    }

    /// Schedule the next presentation (or finish). Also restarts the
    /// current one after a false press.
    fn next_trial(&mut self, cx: &mut Cx) {
        cx.stop_timer(self.test_timer);
        let done = self.test.as_ref().map(|t| t.is_done()).unwrap_or(true);
        if done {
            self.finish_test(cx);
            return;
        }
        self.test_phase = TestPhase::PreDelay;
        let delay = PRE_DELAY_MIN + PRE_DELAY_SPREAD * self.rand_unit();
        self.test_timer = cx.start_timeout(delay);
        self.update_test_ui(cx);
    }

    fn current_presentation(&self) -> Option<Presentation> {
        self.test.as_ref().and_then(|t| t.current())
    }

    fn on_test_timer(&mut self, cx: &mut Cx) {
        match self.test_phase {
            TestPhase::Idle => {}
            TestPhase::PreDelay => {
                let Some(p) = self.current_presentation() else {
                    self.finish_test(cx);
                    return;
                };
                let amp = db_hl_to_amp(p.band, p.db_hl, self.test_offset_db(cx));
                self.tone.play(p.freq_hz(), amp, p.ear);
                self.test_phase = TestPhase::Playing;
                self.test_timer = cx.start_timeout(BURST_SECS as f64);
            }
            TestPhase::Playing => {
                self.tone.stop();
                self.test_phase = TestPhase::Window;
                self.test_timer = cx.start_timeout(RESPONSE_WINDOW);
            }
            TestPhase::Window => {
                if let Some(t) = &mut self.test {
                    t.respond(false);
                }
                self.next_trial(cx);
            }
        }
    }

    fn heard_pressed(&mut self, cx: &mut Cx) {
        match self.test_phase {
            TestPhase::Idle => {}
            TestPhase::PreDelay => {
                // Nothing was playing: count it and re-randomise the delay.
                self.false_presses += 1;
                self.next_trial(cx);
            }
            TestPhase::Playing | TestPhase::Window => {
                self.tone.stop();
                if let Some(t) = &mut self.test {
                    t.respond(true);
                }
                self.next_trial(cx);
            }
        }
    }

    fn finish_test(&mut self, cx: &mut Cx) {
        let res = self.test.as_ref().and_then(|t| t.result());
        self.end_test(cx);
        if let Some((ag, notes)) = &res {
            let mut text = String::from("Result (dB HL, approximate)\n");
            for (i, hz) in BANDS_HZ.iter().enumerate() {
                text.push_str(&format!("{hz:>5} Hz    L {:>3}    R {:>3}\n", format_db(ag.left[i]), format_db(ag.right[i])));
            }
            if self.false_presses > 0 {
                text.push_str(&format!("\n{} press(es) while no tone was playing.", self.false_presses));
            }
            for n in notes {
                text.push_str("\n");
                text.push_str(n);
            }
            self.ui.label(cx, ids!(ht_results_label)).set_text(cx, &text);
            self.push_log(cx, "hearing test finished".into());
        }
        self.test_result = res;
        self.update_test_ui(cx);
    }

    /// Stop everything and put the buds back; used by Stop, finish, an
    /// audio-device loss and shutdown.
    fn end_test(&mut self, cx: &mut Cx) {
        self.tone.stop();
        cx.stop_timer(self.test_timer);
        self.test_timer = Timer::default();
        self.test_phase = TestPhase::Idle;
        self.test = None;
        self.manual_playing = None;
        self.restore_buds(cx);
        self.update_test_ui(cx);
    }

    /// Show or hide the manual (slider) sub-page. The buds are isolated
    /// while it is shown, like during the automatic test.
    fn set_manual_mode(&mut self, cx: &mut Cx, on: bool) {
        if on && self.test_running() {
            // A disabled CheckBox still takes the click, so revert it.
            self.ui.check_box(cx, ids!(ht_manual_toggle)).set_active(cx, false, Animate::No);
            self.push_log(cx, "stop the running test before switching to manual mode".into());
            self.ui.redraw(cx);
            return;
        }
        self.tone.stop();
        self.manual_playing = None;
        self.manual_mode = on;
        self.ui.view(cx, ids!(ht_auto_view)).set_visible(cx, !on);
        self.ui.view(cx, ids!(ht_manual_view)).set_visible(cx, on);
        if on {
            self.isolate_buds();
            self.push_log(cx, "manual hearing test mode on".into());
        } else {
            if !self.test_running() {
                self.restore_buds(cx);
            }
            self.push_log(cx, "manual hearing test mode off".into());
        }
        self.ui.redraw(cx);
    }

    /// Audiogram from the 16 manual sliders, clamped to the AirPods' range
    /// (the sliders go down to -10 dB HL).
    fn manual_audiogram(&self, cx: &mut Cx) -> Audiogram {
        let mut ag = Audiogram::default();
        for band in 0..8 {
            let l = self.ui.slider(cx, manual_left_ids()[band]).value().unwrap_or(0.0) as f32;
            let r = self.ui.slider(cx, manual_right_ids()[band]).value().unwrap_or(0.0) as f32;
            ag.left[band] = l.clamp(DB_HL_MIN, DB_HL_MAX);
            ag.right[band] = r.clamp(DB_HL_MIN, DB_HL_MAX);
        }
        ag
    }

    /// Put `ag` into the Audiogram tab's fields (marked unsaved, so the
    /// next device snapshot does not overwrite them) and switch to that tab.
    fn show_audiogram_in_tab(&mut self, cx: &mut Cx, ag: &Audiogram, status: &str) {
        self.audiogram_to_ui(cx, ag);
        self.audiogram_dirty = true;
        self.ui.label(cx, ids!(ag_status_label)).set_text(cx, status);
        self.show_tab(cx, live_id!(page_audiogram));
    }

    /// Switch to `page` from code, keeping the tab row in step.
    fn show_tab(&mut self, cx: &mut Cx, page: LiveId) {
        // `set_active` does not deselect the sibling tabs.
        for (tab, p) in tabs() {
            self.ui.radio_button(cx, tab).set_active(cx, p == page, Animate::No);
        }
        self.ui.page_flip(cx, ids!(pages)).set_active_page(cx, page);
        self.ui.redraw(cx);
    }

    // ---- setup ----

    /// Open the Setup tab once per launch when something on it is still
    /// to do. Waits for the disclaimer, which covers the window until then.
    fn maybe_open_setup(&mut self, cx: &mut Cx) {
        if self.setup_tab_decided || self.settings.disclaimer_accepted != Some(true) {
            return;
        }
        let Some(host) = &self.host else { return };
        self.setup_tab_decided = true;
        if !host.ready() {
            self.show_tab(cx, live_id!(page_setup));
        }
    }

    fn set_setup_msg(&mut self, cx: &mut Cx, text: &str) {
        self.ui.label(cx, ids!(setup_msg_label)).set_text(cx, text);
    }

    fn set_pair_status(&mut self, cx: &mut Cx, text: &str) {
        self.ui.label(cx, ids!(setup_pair_status)).set_text(cx, text);
    }

    /// Start a `setup.sh` action; the result comes back as a [`SetupDone`] action.
    fn run_setup_action(&mut self, cx: &mut Cx, action: SetupAction) {
        self.setup_busy = true;
        let waiting = match action {
            SetupAction::DesktopEntry => "Adding OpenAirCare to the app menu...",
            _ => "Waiting for your password...",
        };
        self.set_setup_msg(cx, waiting);
        setup::spawn(action, |done| Cx::post_action(done));
        self.update_setup_ui(cx);
    }

    fn on_setup_done(&mut self, cx: &mut Cx, done: SetupDone) {
        self.setup_busy = false;
        let text = match (done.action, done.result) {
            (SetupAction::Apply { grant_access }, Ok(())) => {
                let was_apple = self.host.as_ref().map(|h| h.apple_identity).unwrap_or(false);
                let has_paired = self.host.as_ref().map(|h| !h.paired_airpods.is_empty()).unwrap_or(false);
                self.repair_hint = !was_apple && has_paired;
                self.send(Command::CheckSetup);
                if grant_access {
                    "Bluetooth is set up. Close OpenAirCare and open it again so it can use Bluetooth.".to_string()
                } else {
                    "Bluetooth is set up.".to_string()
                }
            }
            (SetupAction::Undo, Ok(())) => {
                self.repair_hint = false;
                self.send(Command::CheckSetup);
                "Bluetooth is back to how it was before setup. Pair the AirPods again if they stop connecting.".to_string()
            }
            (SetupAction::DesktopEntry, Ok(())) => "OpenAirCare is in the app menu.".to_string(),
            (_, Err(e)) => e,
        };
        self.push_log(cx, format!("setup: {}", text.lines().next().unwrap_or_default()));
        self.set_setup_msg(cx, &text);
        self.update_setup_ui(cx);
    }

    fn on_setup_event(&mut self, cx: &mut Cx, ev: SetupEvent) {
        match ev {
            SetupEvent::Status(host) => {
                if host.paired_airpods.is_empty() {
                    self.repair_hint = false;
                }
                self.host = Some(host);
                self.maybe_open_setup(cx);
            }
            SetupEvent::ScanResult(result) => {
                self.scanning = false;
                self.found.clear();
                match result {
                    Ok(devices) if devices.is_empty() => {
                        self.set_pair_status(cx, "No AirPods found. Check that the case light is flashing white, then search again.");
                    }
                    Ok(devices) => {
                        self.found = devices.into_iter().take(found_ids().len()).collect();
                        self.set_pair_status(cx, "Press the AirPods you want to pair.");
                    }
                    Err(e) => self.set_pair_status(cx, &format!("The search failed: {e}")),
                }
            }
            SetupEvent::PairResult { mac, result } => {
                self.pairing = false;
                match result {
                    Ok(()) => {
                        self.found.clear();
                        self.repair_hint = false;
                        self.set_pair_status(cx, "Paired. Wear the AirPods: OpenAirCare is connecting (see the Status tab).");
                        self.push_log(cx, format!("paired with {mac}"));
                        self.send(Command::Connect(Some(mac)));
                    }
                    Err(e) => self.set_pair_status(
                        cx,
                        &format!("{e}. Hold the case button until the light flashes white, then search again."),
                    ),
                }
            }
            SetupEvent::ForgetResult { mac, result } => match result {
                Ok(()) => {
                    self.push_log(cx, format!("removed the pairing with {mac}"));
                    self.set_pair_status(cx, "Pairing removed. Put the AirPods in pairing mode, then search.");
                }
                Err(e) => self.set_pair_status(cx, &format!("Could not remove the pairing: {e}")),
            },
        }
        self.update_setup_ui(cx);
    }

    /// Refresh the Setup tab from `host` and the scan / pairing state.
    fn update_setup_ui(&mut self, cx: &mut Cx) {
        let (bt_text, needs_apply, adapter_ok) = match &self.host {
            None => ("Checking Bluetooth...".to_string(), false, false),
            Some(h) => match (&h.adapter_error, h.apple_identity, &h.socket_error) {
                (Some(e), _, _) => (
                    format!("No Bluetooth adapter was found ({e}). Check that Bluetooth is installed (package bluez) and switched on, then press Check again."),
                    false,
                    false,
                ),
                (None, false, _) => (
                    "To do: this computer does not identify as an Apple device yet. AirPods only open their Hearing Health settings to one.".to_string(),
                    true,
                    true,
                ),
                (None, true, Some(e)) => (
                    format!("To do: OpenAirCare is not allowed to open a Bluetooth connection ({e})."),
                    true,
                    true,
                ),
                (None, true, None) => ("Done: Bluetooth identifies as an Apple device.".to_string(), false, true),
            },
        };
        self.ui.label(cx, ids!(setup_bt_label)).set_text(cx, &bt_text);
        self.ui.button(cx, ids!(setup_apply_btn)).set_enabled(cx, needs_apply && !self.setup_busy);
        self.ui.button(cx, ids!(setup_undo_btn)).set_enabled(cx, !self.setup_busy);
        self.ui.button(cx, ids!(setup_menu_btn)).set_enabled(cx, !self.setup_busy);

        let paired = self.host.as_ref().map(|h| h.paired_airpods.clone()).unwrap_or_default();
        let pair_text = if self.pairing {
            "Pairing... keep the lid open.".to_string()
        } else if self.scanning {
            "Searching for about 15 seconds. Keep the lid open.".to_string()
        } else if let Some(d) = paired.first() {
            if self.repair_hint {
                format!(
                    "To do: {} ({}) was paired before this computer identified as Apple, and still remembers the old identity. Remove the pairing, then pair again.",
                    d.name, d.mac
                )
            } else {
                format!("Done: paired with {} ({}).", d.name, d.mac)
            }
        } else if adapter_ok {
            "To do: no AirPods are paired with this computer yet.".to_string()
        } else {
            String::new()
        };
        self.ui.label(cx, ids!(setup_pair_label)).set_text(cx, &pair_text);
        let idle = adapter_ok && !self.scanning && !self.pairing;
        self.ui.button(cx, ids!(setup_scan_btn)).set_enabled(cx, idle);
        self.ui.button(cx, ids!(setup_forget_btn)).set_enabled(cx, idle && !paired.is_empty());
        for (i, id) in found_ids().iter().enumerate() {
            let button = self.ui.button(cx, id);
            match self.found.get(i) {
                Some(d) => {
                    button.set_text(cx, &format!("Pair {} ({})", d.name, d.mac));
                    button.set_visible(cx, true);
                    button.set_enabled(cx, idle);
                }
                None => button.set_visible(cx, false),
            }
        }
        self.ui.redraw(cx);
    }

    /// Manual-mode slider events: hold to play that band in that ear,
    /// release to stop.
    fn handle_manual_sliders(&mut self, cx: &mut Cx, actions: &Actions) {
        let offset = self.test_offset_db(cx);
        for (ear, ids) in [(Ear::Left, manual_left_ids()), (Ear::Right, manual_right_ids())] {
            for (band, id) in ids.iter().enumerate() {
                let slider = self.ui.slider(cx, id);
                let freq = BANDS_HZ[band] as f32;
                match actions.find_widget_action(slider.widget_uid()).cast::<SliderAction>() {
                    SliderAction::StartSlide if self.manual_mode => {
                        let db_hl = slider.value().unwrap_or(0.0) as f32;
                        self.tone.play_looping(freq, db_hl_to_amp(band, db_hl, offset), ear);
                        self.manual_playing = Some((ear, band));
                    }
                    SliderAction::Slide(v) if self.manual_playing == Some((ear, band)) => {
                        self.tone.set_amp(db_hl_to_amp(band, v as f32, offset));
                    }
                    SliderAction::EndSlide(_) => {
                        self.tone.stop();
                        self.manual_playing = None;
                    }
                    SliderAction::TextSlide(v) if self.manual_mode => {
                        // Typed value: one burst so it can be checked.
                        self.tone.play(freq, db_hl_to_amp(band, v as f32, offset), ear);
                    }
                    _ => {}
                }
            }
        }
    }

    fn update_test_ui(&mut self, cx: &mut Cx) {
        let running = self.test_running();
        let progress = match self.current_presentation() {
            Some(p) => {
                let (done, total) = self.test.as_ref().map(|t| t.progress()).unwrap_or((0, 0));
                let ear = if p.ear == Ear::Left { "Left" } else { "Right" };
                format!("{ear} ear  -  {} Hz  -  band {} of {total}", p.freq_hz() as u32, done + 1)
            }
            None => String::new(),
        };
        self.ui.label(cx, ids!(ht_progress_label)).set_text(cx, &progress);
        // The same text through every phase, so it gives no cue that a tone
        // has started.
        let state = if running {
            "Press the button (or Space) as soon as you hear the beeps."
        } else if self.test_result.is_some() {
            "Test complete. Review the result below, then use it in the Audiogram tab."
        } else {
            "Not running."
        };
        self.ui.label(cx, ids!(ht_state_label)).set_text(cx, state);
        let false_text = if running && self.false_presses > FALSE_PRESS_WARN {
            format!(
                "{} presses while nothing was playing. Wait for the beeps; pressing early makes the result unreliable.",
                self.false_presses
            )
        } else {
            String::new()
        };
        self.ui.label(cx, ids!(ht_false_label)).set_text(cx, &false_text);
        self.ui.button(cx, ids!(ht_heard_btn)).set_enabled(cx, running);
        self.ui.button(cx, ids!(ht_stop_btn)).set_enabled(cx, running);
        self.ui.button(cx, ids!(ht_start_btn)).set_enabled(cx, !running);
        self.ui.button(cx, ids!(ht_sample_btn)).set_enabled(cx, !running);
        self.ui.button(cx, ids!(ht_use_btn)).set_enabled(cx, self.test_result.is_some());
        self.ui.check_box(cx, ids!(ht_manual_toggle)).set_disabled(cx, running);
        self.ui.redraw(cx);
    }

    fn set_mode_radios(&mut self, cx: &mut Cx, mode: Option<ListeningMode>) {
        let ids: [(&[LiveId], ListeningMode); 4] = [
            (ids!(mode_off), ListeningMode::Off),
            (ids!(mode_anc), ListeningMode::NoiseCancellation),
            (ids!(mode_transparency), ListeningMode::Transparency),
            (ids!(mode_adaptive), ListeningMode::Adaptive),
        ];
        for (id, m) in ids {
            self.ui.radio_button(cx, id).set_active(cx, mode == Some(m), Animate::No);
        }
    }

    /// Refresh every widget from the latest snapshot.
    fn sync_ui(&mut self, cx: &mut Cx) {
        let Some(snap) = self.snap.clone() else { return };

        let state_text = match &snap.state {
            ConnectionState::Disconnected => "Disconnected".to_string(),
            ConnectionState::Connecting(stage) => format!("Connecting: {stage}"),
            ConnectionState::Connected => format!("Connected via {}", snap.backend),
            ConnectionState::Reconnecting { attempt, in_secs } => {
                format!("Retrying in {in_secs}s (attempt {attempt})")
            }
            ConnectionState::Error(e) => format!("Error: {e}"),
        };
        self.ui.label(cx, ids!(state_label)).set_text(cx, &state_text);
        let error_text = match (&snap.state, &snap.last_error) {
            (ConnectionState::Error(_), _) | (_, None) => String::new(),
            (_, Some(e)) => format!("Last error: {e}"),
        };
        self.ui.label(cx, ids!(error_label)).set_text(cx, &error_text);

        let device_text = match (&snap.device, &snap.info) {
            (Some(d), Some(i)) => format!(
                "{} [{}] - {} {} - firmware {}",
                i.name,
                d.mac,
                model::family_name(&i.model_number),
                i.model_number,
                i.firmware_version
            ),
            (Some(d), None) => format!("{} [{}]", d.name, d.mac),
            _ => "No device".to_string(),
        };
        self.ui.label(cx, ids!(device_label)).set_text(cx, &device_text);

        let battery_text = if snap.battery.is_empty() {
            String::new()
        } else {
            snap.battery
                .iter()
                .map(|b| {
                    use airpods_proto::aacp::battery::BatteryStatus;
                    match b.status {
                        BatteryStatus::Charging => format!("{} {}% (charging)", b.component.label(), b.level),
                        // Case lid closed / bud not in the case: the level byte is meaningless.
                        BatteryStatus::Disconnected => format!("{} --", b.component.label()),
                        _ => format!("{} {}%", b.component.label(), b.level),
                    }
                })
                .collect::<Vec<_>>()
                .join("   ")
        };
        self.ui.label(cx, ids!(battery_label)).set_text(cx, &battery_text);

        let connected = matches!(snap.state, ConnectionState::Connected);
        self.ui.button(cx, ids!(connect_btn)).set_enabled(cx, !connected);
        self.ui.button(cx, ids!(disconnect_btn)).set_enabled(cx, snap.state != ConnectionState::Disconnected);

        self.set_mode_radios(cx, snap.listening_mode);

        // Hearing health page
        let capability_text = match (connected, snap.hearing_aid_capable, &snap.info) {
            (false, _, _) => "Connect your AirPods first (Status tab).".to_string(),
            (true, Some(true), Some(i)) => format!("{} ({}) supports Hearing Health.", model::family_name(&i.model_number), i.model_number),
            (true, Some(false), Some(i)) => format!("{} ({}) does not support Hearing Health (AirPods Pro 2 / Pro 3 only).", i.name, i.model_number),
            (true, _, _) => "Waiting for device information...".to_string(),
        };
        self.ui.label(cx, ids!(capability_label)).set_text(cx, &capability_text);
        self.ui.check_box(cx, ids!(ha_toggle)).set_active(cx, snap.hearing_aid_enabled.unwrap_or(false), Animate::No);
        self.ui.check_box(cx, ids!(swipe_toggle)).set_active(cx, snap.gain_swipe.unwrap_or(false), Animate::No);
        let ha_text = match (&snap.hearing_aid_enabled, snap.hearing_aid_enrolled) {
            (Some(true), _) => "Hearing Health is ON.",
            (Some(false), true) => "Hearing Health is off (audiogram enrolled).",
            (Some(false), false) => "Hearing Health is off.",
            (None, _) => "Hearing Health state unknown yet.",
        };
        self.ui.check_box(cx, ids!(ha_toggle)).set_text(&format!("Hearing Health enabled - {ha_text}"));

        let att_text = if !connected {
            String::new()
        } else if snap.att_ok {
            match snap.data {
                Some(_) => "Hearing Health settings channel (ATT) open; audiogram and adjustments are live.".to_string(),
                None => "ATT channel open, waiting for Hearing Health data...".to_string(),
            }
        } else {
            format!(
"Hearing Health settings channel unavailable: {}\n\nOpen the Setup tab and finish the steps there, then pair the AirPods again. Close any other app that is talking to them.",
                snap.att_error.clone().unwrap_or_default()
            )
        };
        self.ui.label(cx, ids!(att_label)).set_text(cx, &att_text);
        let audiogram_is_flat_zero = snap
            .data
            .map(|d| d.left.eq.iter().chain(d.right.eq.iter()).all(|v| v.abs() < 0.5))
            .unwrap_or(false);
        let adj_hint = if !(snap.att_ok && snap.data.is_some()) {
            "Not available until the Hearing Health settings channel is open (see Hearing Health tab)."
        } else if audiogram_is_flat_zero {
            "The audiogram on the AirPods is all zeros (no hearing loss), so amplification has nothing to amplify: enter your audiogram first (Audiogram tab). Changes are written as you move the sliders."
        } else if snap.hearing_aid_enabled != Some(true) {
            "Changes are written as you move the sliders, but you will only hear them once Hearing Health is on (Hearing Health tab) and the buds are in Transparency mode."
        } else {
            "Changes are written to the AirPods as you move the sliders."
        };
        self.ui.label(cx, ids!(adj_hint_label)).set_text(cx, adj_hint);

        let buds_text = if connected {
            "During the test, and while manual mode is on, the AirPods are switched to Noise Cancellation and Hearing Health is turned off, so the existing profile does not colour the tones. Both are restored afterwards."
        } else {
            "AirPods not connected: the test still plays through the current audio output, but nothing is switched on the buds."
        };
        self.ui.label(cx, ids!(ht_buds_label)).set_text(cx, buds_text);

        if let Some(d) = snap.data {
            self.adjustments_to_ui(cx, &d.adjustments());
            self.ui.slider(cx, ids!(own_voice_slider)).set_value(cx, d.own_voice_amplification as f64);
            if !self.audiogram_dirty {
                self.audiogram_to_ui(cx, &d.audiogram());
            }
        }

        self.ui.check_box(cx, ids!(auto_reconnect_check)).set_active(cx, snap.auto_reconnect, Animate::No);
        self.ui.redraw(cx);
    }
}

fn parse_db(s: &str) -> Result<f32, String> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(0.0);
    }
    let v = t.parse::<f32>().map_err(|_| format!("'{t}' is not a number"))?;
    // The proto layer clamps again on encode; rejecting here tells the user
    // instead of silently sending a different value.
    if !v.is_finite() || !(DB_HL_MIN..=DB_HL_MAX).contains(&v) {
        return Err(format!("'{t}' is outside {DB_HL_MIN:.0}-{DB_HL_MAX:.0} dB HL"));
    }
    Ok(v)
}

fn format_db(v: f32) -> String {
    if (v - v.round()).abs() < 1e-3 {
        format!("{}", v.round() as i32)
    } else {
        format!("{v:.1}")
    }
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).try_init();
        self.settings = Settings::load();

        self.ui.radio_button(cx, ids!(tab_status)).set_active(cx, true, Animate::No);
        if let Some(mac) = &self.settings.device_mac {
            self.ui.text_input(cx, ids!(mac_input)).set_text(cx, mac);
        }
        if let Some(ag) = &self.settings.audiogram.clone() {
            self.audiogram_to_ui(cx, ag);
        }
        if let Some(off) = self.settings.hearing_test_offset_db {
            self.ui.slider(cx, ids!(ht_offset_slider)).set_value(cx, off as f64);
        }
        tone::install(cx, self.tone.clone());
        self.update_test_ui(cx);
        self.update_setup_ui(cx);

        let emit: airpods_link::session::Emit = Arc::new(|e: SessionEvent| Cx::post_action(e));
        let handle = airpods_link::session::spawn(make_transport(), emit);
        if let Some(auto) = self.settings.auto_reconnect {
            handle.send(Command::SetAutoReconnect(auto));
        }
        handle.send(Command::CheckSetup);
        // First launch: nothing talks to the buds until the disclaimer is
        // acknowledged. The modal cannot be dismissed and covers the UI.
        if self.settings.disclaimer_accepted == Some(true) {
            handle.send(Command::Connect(self.settings.device_mac.clone()));
        } else {
            self.ui.modal(cx, ids!(disclaimer_modal)).open(cx);
        }
        self.session = Some(handle);
        self.push_log(cx, "started".into());
    }

    fn handle_audio_devices(&mut self, cx: &mut Cx, e: &AudioDevicesEvent) {
        // Prefer the AirPods' A2DP sink; `match_outputs` falls back to the
        // default output when no name matches.
        let ids = e.match_outputs(&["AirPods", "airpods", "AirPod"]);
        let chosen = e.descs.iter().find(|d| Some(&d.device_id) == ids.first());
        let text = match chosen {
            Some(d) if d.name.to_ascii_lowercase().contains("airpod") => format!("Audio output: {}", d.name),
            Some(d) => format!("Audio output: {} (not the AirPods: select them as the Bluetooth audio sink first)", d.name),
            None => "Audio output: none available".to_string(),
        };
        self.ui.label(cx, ids!(ht_device_label)).set_text(cx, &text);
        cx.use_audio_outputs(&ids);
        if ids.is_empty() && self.test_running() {
            self.end_test(cx);
            self.push_log(cx, "hearing test stopped: audio output lost".into());
        } else if ids.is_empty() && self.manual_playing.is_some() {
            self.tone.stop();
            self.manual_playing = None;
        }
        self.ui.redraw(cx);
    }

    fn handle_timer(&mut self, cx: &mut Cx, e: &TimerEvent) {
        if self.test_timer.is_timer(e).is_some() {
            self.on_test_timer(cx);
        }
    }

    fn handle_key_down(&mut self, cx: &mut Cx, e: &KeyEvent) {
        if e.key_code == KeyCode::Space && !e.is_repeat && self.test_running() {
            self.heard_pressed(cx);
        }
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        // ---- events from the session thread ----
        let mut got_snapshot = false;
        for action in actions {
            if let Some(ev) = action.downcast_ref::<SessionEvent>() {
                match ev {
                    SessionEvent::Snapshot(s) => {
                        self.snap = Some(s.clone());
                        got_snapshot = true;
                    }
                    SessionEvent::Log(l) => {
                        let line = l.clone();
                        self.push_log(cx, line);
                    }
                    SessionEvent::Setup(ev) => {
                        let ev = ev.clone();
                        self.on_setup_event(cx, ev);
                    }
                }
            }
            // ---- result of a `setup.sh` action ----
            if let Some(done) = action.downcast_ref::<SetupDone>() {
                let done = done.clone();
                self.on_setup_done(cx, done);
            }
        }
        if got_snapshot {
            self.sync_ui(cx);
        }

        // ---- first-launch disclaimer ----
        if self.ui.button(cx, ids!(disclaimer_accept_btn)).clicked(actions) {
            self.settings.disclaimer_accepted = Some(true);
            self.settings.save();
            self.ui.modal(cx, ids!(disclaimer_modal)).close(cx);
            self.push_log(cx, "disclaimer acknowledged".into());
            self.send(Command::Connect(self.settings.device_mac.clone()));
            self.maybe_open_setup(cx);
            self.ui.redraw(cx);
        }
        if self.ui.button(cx, ids!(disclaimer_decline_btn)).clicked(actions) {
            // Not accepted: nothing was written to the buds or to disk.
            std::process::exit(0);
        }

        // ---- tabs ----
        if let Some(i) = self
            .ui
            .radio_button_set(cx, ids_list!(tab_setup, tab_status, tab_hearing, tab_test, tab_audiogram, tab_adjust))
            .selected(cx, actions)
        {
            let page = tabs()[i].1;
            self.ui.page_flip(cx, ids!(pages)).set_active_page(cx, page);
            self.ui.redraw(cx);
        }

        // ---- setup page ----
        if self.ui.button(cx, ids!(setup_apply_btn)).clicked(actions) && !self.setup_busy {
            let grant_access = self.host.as_ref().map(|h| h.socket_error.is_some()).unwrap_or(false);
            self.run_setup_action(cx, SetupAction::Apply { grant_access });
        }
        if self.ui.button(cx, ids!(setup_undo_btn)).clicked(actions) && !self.setup_busy {
            self.run_setup_action(cx, SetupAction::Undo);
        }
        if self.ui.button(cx, ids!(setup_menu_btn)).clicked(actions) && !self.setup_busy {
            self.run_setup_action(cx, SetupAction::DesktopEntry);
        }
        if self.ui.button(cx, ids!(setup_check_btn)).clicked(actions) {
            self.send(Command::CheckSetup);
        }
        if self.ui.button(cx, ids!(setup_scan_btn)).clicked(actions) && !self.scanning && !self.pairing {
            self.scanning = true;
            self.found.clear();
            self.set_pair_status(cx, "");
            self.send(Command::ScanForAirpods);
            self.update_setup_ui(cx);
        }
        if self.ui.button(cx, ids!(setup_forget_btn)).clicked(actions) && !self.scanning && !self.pairing {
            let paired = self.host.as_ref().map(|h| h.paired_airpods.clone()).unwrap_or_default();
            for d in paired {
                self.send(Command::Forget(d.mac));
            }
        }
        for (i, id) in found_ids().iter().enumerate() {
            if self.ui.button(cx, id).clicked(actions) && !self.pairing {
                if let Some(d) = self.found.get(i).cloned() {
                    self.pairing = true;
                    self.set_pair_status(cx, "");
                    self.send(Command::Pair(d.mac));
                    self.update_setup_ui(cx);
                }
            }
        }

        // ---- status page ----
        if self.ui.button(cx, ids!(connect_btn)).clicked(actions) {
            let mac = self.mac_from_input(cx);
            self.settings.device_mac = mac.clone();
            self.settings.save();
            self.send(Command::Connect(mac));
        }
        if self.ui.button(cx, ids!(disconnect_btn)).clicked(actions) {
            self.send(Command::Disconnect);
        }
        if self.ui.button(cx, ids!(refresh_btn)).clicked(actions) {
            self.send(Command::RefreshDevices);
        }
        if let Some(v) = self.ui.check_box(cx, ids!(auto_reconnect_check)).changed(actions) {
            self.settings.auto_reconnect = Some(v);
            self.settings.save();
            self.send(Command::SetAutoReconnect(v));
        }
        if let Some(i) = self
            .ui
            .radio_button_set(cx, ids_list!(mode_off, mode_anc, mode_transparency, mode_adaptive))
            .selected(cx, actions)
        {
            self.send(Command::SetListeningMode(ListeningMode::ALL[i]));
        }

        // ---- hearing health page ----
        if let Some(on) = self.ui.check_box(cx, ids!(ha_toggle)).changed(actions) {
            self.send(Command::SetHearingAid(on));
        }
        if let Some(on) = self.ui.check_box(cx, ids!(swipe_toggle)).changed(actions) {
            self.send(Command::SetGainSwipe(on));
        }

        // ---- audiogram page ----
        for id in left_ids().iter().chain(right_ids().iter()) {
            if self.ui.text_input(cx, id).changed(actions).is_some() {
                self.audiogram_dirty = true;
            }
        }
        if self.ui.button(cx, ids!(ag_apply_btn)).clicked(actions) {
            match self.audiogram_from_ui(cx) {
                Ok(ag) => {
                    self.settings.audiogram = Some(ag);
                    self.settings.save();
                    self.audiogram_dirty = false;
                    self.send(Command::SetAudiogram(ag));
                    self.ui.label(cx, ids!(ag_status_label)).set_text(cx, "Audiogram sent to AirPods (and saved locally).");
                }
                Err(e) => self.ui.label(cx, ids!(ag_status_label)).set_text(cx, &format!("Invalid value: {e}")),
            }
        }
        if self.ui.button(cx, ids!(ag_reload_btn)).clicked(actions) {
            self.audiogram_dirty = false;
            self.send(Command::Reload);
            self.ui.label(cx, ids!(ag_status_label)).set_text(cx, "Reloading from AirPods...");
        }
        if self.ui.button(cx, ids!(ag_save_btn)).clicked(actions) {
            match self.audiogram_from_ui(cx) {
                Ok(ag) => {
                    self.settings.audiogram = Some(ag);
                    self.settings.save();
                    self.ui
                        .label(cx, ids!(ag_status_label))
                        .set_text(cx, &format!("Saved to {}", settings::settings_path().display()));
                }
                Err(e) => self.ui.label(cx, ids!(ag_status_label)).set_text(cx, &format!("Invalid value: {e}")),
            }
        }
        if self.ui.button(cx, ids!(ag_load_btn)).clicked(actions) {
            match self.settings.audiogram {
                Some(ag) => {
                    self.audiogram_to_ui(cx, &ag);
                    self.audiogram_dirty = true; // keep until applied
                    self.ui.label(cx, ids!(ag_status_label)).set_text(cx, "Loaded saved audiogram; press Apply to send it.");
                }
                None => self.ui.label(cx, ids!(ag_status_label)).set_text(cx, "No saved audiogram yet."),
            }
        }

        // ---- hearing test page ----
        if let Some(v) = self.ui.slider(cx, ids!(ht_offset_slider)).end_slide(actions) {
            self.settings.hearing_test_offset_db = Some(v as f32);
            self.settings.save();
        }
        if self.ui.button(cx, ids!(ht_sample_btn)).clicked(actions) && !self.test_running() {
            let amp = db_hl_to_amp(2, 40.0, self.test_offset_db(cx));
            self.tone.play(1000.0, amp, Ear::Both);
        }
        if self.ui.button(cx, ids!(ht_start_btn)).clicked(actions) && !self.test_running() {
            self.ui.modal(cx, ids!(ht_warn_modal)).open(cx);
            self.ui.redraw(cx);
        }
        if self.ui.button(cx, ids!(ht_warn_cancel_btn)).clicked(actions) {
            self.ui.modal(cx, ids!(ht_warn_modal)).close(cx);
            self.ui.redraw(cx);
        }
        if self.ui.button(cx, ids!(ht_warn_start_btn)).clicked(actions) {
            self.ui.modal(cx, ids!(ht_warn_modal)).close(cx);
            self.begin_test(cx);
        }
        if self.ui.button(cx, ids!(ht_stop_btn)).clicked(actions) && self.test_running() {
            self.end_test(cx);
            self.push_log(cx, "hearing test stopped".into());
        }
        if self.ui.button(cx, ids!(ht_heard_btn)).clicked(actions) {
            self.heard_pressed(cx);
        }
        if self.ui.button(cx, ids!(ht_use_btn)).clicked(actions) {
            if let Some((ag, _)) = self.test_result.clone() {
                self.show_audiogram_in_tab(cx, &ag, "Filled from the hearing test (approximate). Review, then press Apply to AirPods.");
            }
        }
        if let Some(on) = self.ui.check_box(cx, ids!(ht_manual_toggle)).changed(actions) {
            self.set_manual_mode(cx, on);
        }
        self.handle_manual_sliders(cx, actions);
        if self.ui.button(cx, ids!(ht_manual_use_btn)).clicked(actions) && self.manual_mode {
            let ag = self.manual_audiogram(cx);
            self.show_audiogram_in_tab(cx, &ag, "Filled from the manual hearing test (approximate). Review, then press Apply to AirPods.");
        }

        // ---- adjustments page ----
        let slid = [ids!(amp_slider), ids!(bal_slider), ids!(tone_slider), ids!(anr_slider)]
            .iter()
            .any(|id| self.ui.slider(cx, *id).slided(actions).is_some());
        let conv_changed = self.ui.check_box(cx, ids!(conv_check)).changed(actions).is_some();
        if slid || conv_changed {
            self.apply_adjustments_from_ui(cx);
        }
        if let Some(v) = self.ui.slider(cx, ids!(own_voice_slider)).slided(actions) {
            self.send(Command::SetOwnVoice(v as f32));
        }
        // Ask once the drag (or typed value) is finished, not on every step.
        if let Some(v) = self.ui.slider(cx, ids!(amp_slider)).end_slide(actions) {
            if v as f32 > SAFE_AMP_MAX && !self.amp_boost_confirmed {
                self.amp_boost_pending = true;
                self.ui.label(cx, ids!(amp_warn_value_label)).set_text(
                    cx,
                    &format!("You are about to set amplification to {v:.2}. Apple's maximum is {SAFE_AMP_MAX:.2}."),
                );
                self.ui.modal(cx, ids!(amp_warn_modal)).open(cx);
                self.ui.redraw(cx);
            }
        }
        if self.ui.button(cx, ids!(amp_boost_confirm_btn)).clicked(actions) {
            self.amp_boost_confirmed = true;
            self.amp_boost_pending = false;
            self.ui.modal(cx, ids!(amp_warn_modal)).close(cx);
            let amp = self.amp_slider_value(cx);
            self.push_log(cx, format!("amplification boosted to {amp:.2}, above Apple's maximum of {SAFE_AMP_MAX:.2} (user confirmed)"));
            self.apply_adjustments_from_ui(cx);
            self.ui.redraw(cx);
        }
        if self.ui.button(cx, ids!(amp_boost_cancel_btn)).clicked(actions) {
            self.amp_boost_pending = false;
            self.ui.modal(cx, ids!(amp_warn_modal)).close(cx);
            self.ui.slider(cx, ids!(amp_slider)).set_value(cx, SAFE_AMP_MAX as f64);
            self.apply_adjustments_from_ui(cx);
            self.ui.redraw(cx);
        }
        if self.ui.button(cx, ids!(reset_btn)).clicked(actions) {
            self.amp_boost_pending = false;
            self.amp_boost_confirmed = false;
            self.ui.modal(cx, ids!(amp_warn_modal)).close(cx);
            self.adjustments_to_ui(cx, &Adjustments::RESET);
            self.settings.adjustments = Some(Adjustments::RESET);
            self.settings.save();
            self.send(Command::ResetAdjustments);
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if let Event::Shutdown = event {
            // Best effort: put the buds back if a test was running.
            self.end_test(cx);
            self.settings.save();
        }
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
