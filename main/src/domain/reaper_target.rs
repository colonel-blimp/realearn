use derive_more::Display;
use enum_iterator::IntoEnumIterator;
use helgoboss_learn::{ControlType, ControlValue, Target, UnitValue};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use reaper_high::{
    Action, ActionCharacter, AvailablePanValue, ChangeEvent, Fx, FxParameter, FxParameterCharacter,
    Pan, PlayRate, Project, Reaper, Tempo, Track, TrackSend, Volume, Width,
};
use reaper_medium::{
    Bpm, CommandId, Db, FxPresetRef, GetParameterStepSizesResult, MasterTrackBehavior,
    NormalizedPlayRate, PlaybackSpeedFactor, ReaperNormalizedFxParamValue, ReaperPanValue,
    ReaperWidthValue, UndoBehavior,
};
use rx_util::{BoxedUnitEvent, Event, UnitEvent};
use rxrust::prelude::*;

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use slog::warn;

use crate::core::Global;
use crate::domain::ui_util::{
    format_as_double_percentage_without_unit, format_as_percentage_without_unit,
    format_as_symmetric_percentage_without_unit, parse_from_double_percentage,
    parse_from_symmetric_percentage, parse_unit_value_from_percentage,
};
use crate::domain::{AdditionalFeedbackEvent, DomainGlobal, RealearnTarget};
use std::convert::TryInto;
use std::rc::Rc;

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum TargetCharacter {
    Trigger,
    Switch,
    Discrete,
    Continuous,
    VirtualMulti,
    VirtualButton,
}

impl TargetCharacter {
    pub fn from_control_type(control_type: ControlType) -> TargetCharacter {
        use ControlType::*;
        match control_type {
            AbsoluteTrigger => TargetCharacter::Trigger,
            AbsoluteSwitch => TargetCharacter::Switch,
            AbsoluteContinuous | AbsoluteContinuousRoundable { .. } => TargetCharacter::Continuous,
            AbsoluteDiscrete { .. } | Relative => TargetCharacter::Discrete,
            VirtualMulti => TargetCharacter::VirtualMulti,
            VirtualButton => TargetCharacter::VirtualButton,
        }
    }
}

/// This is a ReaLearn target.
///
/// Unlike TargetModel, the real target has everything resolved already (e.g. track and FX) and
/// is immutable.
//
// When adding a new target type, please proceed like this:
//
// 1. Recompile and see what fails.
//      - Yes, we basically let the compiler write our to-do list :)
//      - For this to work, we must take care not to use `_` when doing pattern matching on
//        `ReaperTarget`, but instead mention each variant explicitly.
// 2. One situation where this doesn't work is when we use `matches!`. So after that, just search
//    for occurrences of `matches!` in this file and do what needs to be done!
// 3. To not miss anything, look for occurrences of `TrackVolume` (as a good example).
#[derive(Clone, Debug, PartialEq)]
pub enum ReaperTarget {
    Action {
        action: Action,
        invocation_type: ActionInvocationType,
        project: Project,
    },
    FxParameter {
        param: FxParameter,
    },
    TrackVolume {
        track: Track,
    },
    TrackSendVolume {
        send: TrackSend,
    },
    TrackPan {
        track: Track,
    },
    TrackWidth {
        track: Track,
    },
    TrackArm {
        track: Track,
    },
    TrackSelection {
        track: Track,
        select_exclusively: bool,
    },
    TrackMute {
        track: Track,
    },
    TrackSolo {
        track: Track,
    },
    TrackSendPan {
        send: TrackSend,
    },
    TrackSendMute {
        send: TrackSend,
    },
    Tempo {
        project: Project,
    },
    Playrate {
        project: Project,
    },
    FxEnable {
        fx: Fx,
    },
    FxPreset {
        fx: Fx,
    },
    SelectedTrack {
        project: Project,
    },
    AllTrackFxEnable {
        track: Track,
    },
    Transport {
        project: Project,
        action: TransportAction,
    },
    LoadFxSnapshot {
        fx: Fx,
        chunk: Rc<String>,
        chunk_hash: u64,
    },
}

impl RealearnTarget for ReaperTarget {
    fn character(&self) -> TargetCharacter {
        TargetCharacter::from_control_type(self.control_type())
    }

    fn open(&self) {
        if let ReaperTarget::Action {
            action: _, project, ..
        } = self
        {
            // Just open action window
            Reaper::get()
                .main_section()
                .action_by_command_id(CommandId::new(40605))
                .invoke_as_trigger(Some(*project));
            return;
        }
        if let Some(fx) = self.fx() {
            fx.show_in_floating_window();
            return;
        }
        if let Some(track) = self.track() {
            track.select_exclusively();
            // Scroll to track
            Reaper::get()
                .main_section()
                .action_by_command_id(CommandId::new(40913))
                .invoke_as_trigger(Some(track.project()));
        }
    }

    fn parse_as_value(&self, text: &str) -> Result<UnitValue, &'static str> {
        use ReaperTarget::*;
        match self {
            TrackVolume { .. } | TrackSendVolume { .. } => parse_value_from_db(text),
            TrackPan { .. } | TrackSendPan { .. } => parse_value_from_pan(text),
            Playrate { .. } => parse_value_from_playback_speed_factor(text),
            Tempo { .. } => parse_value_from_bpm(text),
            FxPreset { .. } | SelectedTrack { .. } => self.parse_value_from_discrete_value(text),
            FxParameter { param } if param.character() == FxParameterCharacter::Discrete => {
                self.parse_value_from_discrete_value(text)
            }
            Action { .. }
            | LoadFxSnapshot { .. }
            | FxParameter { .. }
            | TrackArm { .. }
            | TrackSelection { .. }
            | TrackMute { .. }
            | TrackSolo { .. }
            | TrackSendMute { .. }
            | FxEnable { .. }
            | AllTrackFxEnable { .. }
            | Transport { .. } => parse_unit_value_from_percentage(text),
            TrackWidth { .. } => parse_from_symmetric_percentage(text),
        }
    }

    fn parse_as_step_size(&self, text: &str) -> Result<UnitValue, &'static str> {
        use ReaperTarget::*;
        match self {
            Playrate { .. } => parse_step_size_from_playback_speed_factor(text),
            Tempo { .. } => parse_step_size_from_bpm(text),
            FxPreset { .. } | SelectedTrack { .. } => self.parse_value_from_discrete_value(text),
            FxParameter { param } if param.character() == FxParameterCharacter::Discrete => {
                self.parse_value_from_discrete_value(text)
            }
            Action { .. }
            | LoadFxSnapshot { .. }
            | FxParameter { .. }
            | TrackVolume { .. }
            | TrackSendVolume { .. }
            | TrackPan { .. }
            | TrackArm { .. }
            | TrackSelection { .. }
            | TrackMute { .. }
            | TrackSolo { .. }
            | TrackSendPan { .. }
            | TrackSendMute { .. }
            | FxEnable { .. }
            | AllTrackFxEnable { .. }
            | Transport { .. } => parse_unit_value_from_percentage(text),
            TrackWidth { .. } => parse_from_double_percentage(text),
        }
    }

    fn convert_unit_value_to_discrete_value(&self, input: UnitValue) -> Result<u32, &'static str> {
        if self.control_type().is_relative() {
            // Relative MIDI controllers support a maximum of 63 steps.
            return Ok((input.get() * 63.0).round() as _);
        }
        use ReaperTarget::*;
        let result = match self {
            FxPreset { fx } => convert_unit_value_to_preset_index(fx, input)
                .map(|i| i + 1)
                .unwrap_or(0),
            SelectedTrack { project } => convert_unit_value_to_track_index(*project, input)
                .map(|i| i + 1)
                .unwrap_or(0),
            FxParameter { param } => {
                // Example (target step size = 0.10):
                // - 0    => 0
                // - 0.05 => 1
                // - 0.10 => 1
                // - 0.15 => 2
                // - 0.20 => 2
                let step_size = param.step_size().ok_or("not supported")?;
                (input.get() / step_size).round() as _
            }
            Action { .. }
            | TrackVolume { .. }
            | TrackSendVolume { .. }
            | TrackPan { .. }
            | TrackWidth { .. }
            | TrackArm { .. }
            | TrackSelection { .. }
            | TrackMute { .. }
            | TrackSolo { .. }
            | TrackSendPan { .. }
            | TrackSendMute { .. }
            | Tempo { .. }
            | Playrate { .. }
            | FxEnable { .. }
            | AllTrackFxEnable { .. }
            | LoadFxSnapshot { .. }
            | Transport { .. } => return Err("not supported"),
        };
        Ok(result)
    }

    fn format_value_without_unit(&self, value: UnitValue) -> String {
        use ReaperTarget::*;
        match self {
            TrackVolume { .. } | TrackSendVolume { .. } => format_value_as_db_without_unit(value),
            TrackPan { .. } | TrackSendPan { .. } => format_value_as_pan(value),
            Tempo { .. } => format_value_as_bpm_without_unit(value),
            Playrate { .. } => format_value_as_playback_speed_factor_without_unit(value),
            Action { .. }
            | LoadFxSnapshot { .. }
            | FxParameter { .. }
            | TrackArm { .. }
            | TrackSelection { .. }
            | TrackMute { .. }
            | TrackSolo { .. }
            | TrackSendMute { .. }
            | FxEnable { .. }
            | FxPreset { .. }
            | SelectedTrack { .. }
            | AllTrackFxEnable { .. }
            | Transport { .. } => format_as_percentage_without_unit(value),
            TrackWidth { .. } => format_as_symmetric_percentage_without_unit(value),
        }
    }

    fn format_step_size_without_unit(&self, step_size: UnitValue) -> String {
        use ReaperTarget::*;
        match self {
            Tempo { .. } => format_step_size_as_bpm_without_unit(step_size),
            Playrate { .. } => format_step_size_as_playback_speed_factor_without_unit(step_size),
            Action { .. }
            | LoadFxSnapshot { .. }
            | FxParameter { .. }
            | TrackVolume { .. }
            | TrackSendVolume { .. }
            | TrackPan { .. }
            | TrackArm { .. }
            | TrackSelection { .. }
            | TrackMute { .. }
            | TrackSolo { .. }
            | TrackSendPan { .. }
            | TrackSendMute { .. }
            | FxEnable { .. }
            | FxPreset { .. }
            | SelectedTrack { .. }
            | AllTrackFxEnable { .. }
            | Transport { .. } => format_as_percentage_without_unit(step_size),
            TrackWidth { .. } => format_as_double_percentage_without_unit(step_size),
        }
    }

    fn hide_formatted_value(&self) -> bool {
        use ReaperTarget::*;
        matches!(
            self,
            TrackVolume { .. }
                | TrackSendVolume { .. }
                | TrackPan { .. }
                | TrackWidth { .. }
                | TrackSendPan { .. }
                | Playrate { .. }
                | Tempo { .. }
        )
    }

    fn hide_formatted_step_size(&self) -> bool {
        use ReaperTarget::*;
        matches!(
            self,
            TrackVolume { .. }
                | TrackSendVolume { .. }
                | TrackPan { .. }
                | TrackWidth { .. }
                | TrackSendPan { .. }
                | Playrate { .. }
                | Tempo { .. }
        )
    }

    fn value_unit(&self) -> &'static str {
        use ReaperTarget::*;
        match self {
            TrackVolume { .. } | TrackSendVolume { .. } => "dB",
            Tempo { .. } => "bpm",
            Playrate { .. } => "x",
            Action { .. }
            | LoadFxSnapshot { .. }
            | FxParameter { .. }
            | TrackArm { .. }
            | TrackWidth { .. }
            | TrackSelection { .. }
            | TrackMute { .. }
            | TrackSolo { .. }
            | TrackSendMute { .. }
            | FxEnable { .. }
            | FxPreset { .. }
            | SelectedTrack { .. }
            | AllTrackFxEnable { .. }
            | Transport { .. } => "%",
            TrackPan { .. } | TrackSendPan { .. } => "",
        }
    }

    fn step_size_unit(&self) -> &'static str {
        use ReaperTarget::*;
        match self {
            Tempo { .. } => "bpm",
            Playrate { .. } => "x",
            Action { .. }
            | LoadFxSnapshot { .. }
            | FxParameter { .. }
            | TrackVolume { .. }
            | TrackWidth { .. }
            | TrackSendVolume { .. }
            | TrackArm { .. }
            | TrackSelection { .. }
            | TrackMute { .. }
            | TrackSolo { .. }
            | TrackSendMute { .. }
            | FxEnable { .. }
            | FxPreset { .. }
            | SelectedTrack { .. }
            | AllTrackFxEnable { .. }
            | Transport { .. } => "%",
            TrackPan { .. } | TrackSendPan { .. } => "",
        }
    }

    fn format_value(&self, value: UnitValue) -> String {
        use ReaperTarget::*;
        match self {
            FxParameter { param } => param
                // Even if a REAPER-normalized value can take numbers > 1.0, the usual value range
                // is in fact normalized in the classical sense (unit interval).
                .format_reaper_normalized_value(ReaperNormalizedFxParamValue::new(value.get()))
                .map(|s| s.into_string())
                .unwrap_or_else(|_| self.format_value_generic(value)),
            TrackVolume { .. } | TrackSendVolume { .. } => format_value_as_db(value),
            TrackPan { .. } | TrackSendPan { .. } => format_value_as_pan(value),
            FxEnable { .. }
            | TrackArm { .. }
            | TrackMute { .. }
            | TrackSendMute { .. }
            | TrackSelection { .. }
            | TrackSolo { .. } => format_value_as_on_off(value).to_string(),
            FxPreset { fx } => match convert_unit_value_to_preset_index(fx, value) {
                None => "<No preset>".to_string(),
                Some(i) => (i + 1).to_string(),
            },
            SelectedTrack { project } => match convert_unit_value_to_track_index(*project, value) {
                None => "<Master track>".to_string(),
                Some(i) => (i + 1).to_string(),
            },
            Tempo { .. }
            | Playrate { .. }
            | AllTrackFxEnable { .. }
            | Transport { .. }
            | TrackWidth { .. } => self.format_value_generic(value),
            Action { .. } | LoadFxSnapshot { .. } => "".to_owned(),
        }
    }

    fn control(&self, value: ControlValue) -> Result<(), &'static str> {
        use ControlValue::*;
        use ReaperTarget::*;
        match self {
            Action {
                action,
                invocation_type,
                project,
            } => match value {
                Absolute(v) => match invocation_type {
                    ActionInvocationType::Trigger => {
                        if !v.is_zero() {
                            action.invoke(v.get(), false, Some(*project));
                        }
                    }
                    ActionInvocationType::Absolute => action.invoke(v.get(), false, Some(*project)),
                    ActionInvocationType::Relative => {
                        return Err("relative invocation type can't take absolute values");
                    }
                },
                Relative(i) => {
                    if let ActionInvocationType::Relative = invocation_type {
                        action.invoke(i.get() as f64, true, Some(*project));
                    } else {
                        return Err("relative values need relative invocation type");
                    }
                }
            },
            FxParameter { param } => {
                // It's okay to just convert this to a REAPER-normalized value. We don't support
                // values above the maximum (or buggy plug-ins).
                let v = ReaperNormalizedFxParamValue::new(value.as_absolute()?.get());
                param.set_reaper_normalized_value(v).unwrap();
            }
            TrackVolume { track } => {
                let volume = Volume::from_soft_normalized_value(value.as_absolute()?.get());
                track.set_volume(volume);
            }
            TrackSendVolume { send } => {
                let volume = Volume::from_soft_normalized_value(value.as_absolute()?.get());
                send.set_volume(volume);
            }
            TrackPan { track } => {
                let pan = Pan::from_normalized_value(value.as_absolute()?.get());
                track.set_pan(pan);
            }
            TrackWidth { track } => {
                let width = Width::from_normalized_value(value.as_absolute()?.get());
                track.set_width(width);
            }
            TrackArm { track } => {
                if value.as_absolute()?.is_zero() {
                    track.disarm(false);
                } else {
                    track.arm(false);
                }
            }
            TrackSelection {
                track,
                select_exclusively,
            } => {
                if value.as_absolute()?.is_zero() {
                    track.unselect();
                } else if *select_exclusively {
                    track.select_exclusively();
                } else {
                    track.select();
                }
                track.scroll_mixer();
            }
            TrackMute { track } => {
                if value.as_absolute()?.is_zero() {
                    track.unmute();
                } else {
                    track.mute();
                }
            }
            TrackSolo { track } => {
                if value.as_absolute()?.is_zero() {
                    track.unsolo();
                } else {
                    track.solo();
                }
            }
            TrackSendPan { send } => {
                let pan = Pan::from_normalized_value(value.as_absolute()?.get());
                send.set_pan(pan);
            }
            TrackSendMute { send } => {
                if value.as_absolute()?.is_zero() {
                    send.unmute();
                } else {
                    send.mute();
                }
            }
            Tempo { project } => {
                let tempo = reaper_high::Tempo::from_normalized_value(value.as_absolute()?.get());
                project.set_tempo(tempo, UndoBehavior::OmitUndoPoint);
            }
            Playrate { project } => {
                let play_rate = PlayRate::from_normalized_value(NormalizedPlayRate::new(
                    value.as_absolute()?.get(),
                ));
                project.set_play_rate(play_rate);
            }
            FxEnable { fx } => {
                if value.as_absolute()?.is_zero() {
                    fx.disable();
                } else {
                    fx.enable();
                }
            }
            FxPreset { fx } => {
                let preset_index = convert_unit_value_to_preset_index(fx, value.as_absolute()?);
                let preset_ref = match preset_index {
                    None => FxPresetRef::FactoryPreset,
                    Some(i) => FxPresetRef::Preset(i),
                };
                fx.activate_preset(preset_ref);
            }
            SelectedTrack { project } => {
                let track_index = convert_unit_value_to_track_index(*project, value.as_absolute()?);
                let track = match track_index {
                    None => project.master_track(),
                    Some(i) => project.track_by_index(i).ok_or("track not available")?,
                };
                track.select_exclusively();
            }
            AllTrackFxEnable { track } => {
                if value.as_absolute()?.is_zero() {
                    track.disable_fx();
                } else {
                    track.enable_fx();
                }
            }
            Transport { project, action } => {
                use TransportAction::*;
                let off = value.as_absolute()?.is_zero();
                match action {
                    PlayStop => {
                        if off {
                            project.stop();
                        } else {
                            project.play();
                        }
                    }
                    PlayPause => {
                        if off {
                            project.pause();
                        } else {
                            project.play();
                        }
                    }
                    Record => {
                        if off {
                            Reaper::get().disable_record_in_current_project();
                        } else {
                            Reaper::get().enable_record_in_current_project();
                        }
                    }
                    Repeat => {
                        if off {
                            project.disable_repeat();
                        } else {
                            project.enable_repeat();
                        }
                    }
                };
            }
            LoadFxSnapshot {
                fx,
                chunk,
                chunk_hash,
            } => {
                if !value.as_absolute()?.is_zero() {
                    DomainGlobal::target_context()
                        .borrow_mut()
                        .load_fx_snapshot(fx.clone(), chunk, *chunk_hash)
                }
            }
        };
        Ok(())
    }

    fn can_report_current_value(&self) -> bool {
        true
    }
}

impl ReaperTarget {
    /// Notifies about other events which can affect the resulting `ReaperTarget`.
    ///
    /// The resulting `ReaperTarget` doesn't change only if one of our the model properties changes.
    /// It can also change if a track is removed or FX focus changes. We don't include
    /// those in `changed()` because they are global in nature. If we listen to n targets,
    /// we don't want to listen to those global events n times. Just 1 time is enough!
    pub fn potential_static_change_events() -> impl UnitEvent {
        let rx = Global::control_surface_rx();
        rx
            // Considering fx_focused() as static event is okay as long as we don't have a target
            // which switches focus between different FX. As soon as we have that, we must treat
            // fx_focused() as a dynamic event, like track_selection_changed().
            .fx_focused()
            .map_to(())
            .merge(rx.project_switched().map_to(()))
            .merge(rx.track_added().map_to(()))
            .merge(rx.track_removed().map_to(()))
            .merge(rx.tracks_reordered().map_to(()))
            .merge(rx.track_name_changed().map_to(()))
            .merge(rx.fx_added().map_to(()))
            .merge(rx.fx_removed().map_to(()))
            .merge(rx.fx_reordered().map_to(()))
    }

    pub fn is_potential_static_change_event(evt: &ChangeEvent) -> bool {
        use ChangeEvent::*;
        matches!(
            evt,
            FxFocused(_)
                | ProjectSwitched(_)
                | TrackAdded(_)
                | TrackRemoved(_)
                | TracksReordered(_)
                | TrackNameChanged(_)
                | FxAdded(_)
                | FxRemoved(_)
                | FxReordered(_)
        )
    }

    /// This contains all potential target-changing events which could also be fired by targets
    /// themselves. Be careful with those. Reentrancy very likely.
    ///
    /// Previously we always reacted on selection changes. But this naturally causes issues,
    /// which become most obvious with the "Selected track" target. If we resync all mappings
    /// whenever another track is selected, this happens very often while turning an encoder
    /// that navigates between tracks. This in turn renders throttling functionality
    /// useless (because with a resync all runtime mode state is gone). Plus, reentrancy
    /// issues will arise.
    pub fn potential_dynamic_change_events() -> impl UnitEvent {
        let rx = Global::control_surface_rx();
        rx.track_selected_changed().map_to(())
    }

    pub fn is_potential_dynamic_change_event(evt: &ChangeEvent) -> bool {
        use ChangeEvent::*;
        matches!(evt, TrackSelectedChanged(_))
    }

    /// This is eventually going to replace Rx (touched method), at least for domain layer.
    // TODO-medium Unlike the Rx stuff, this doesn't yet contain "Action touch".
    pub fn touched_from_change_event(evt: ChangeEvent) -> Option<ReaperTarget> {
        use ChangeEvent::*;
        use ReaperTarget::*;
        let target = match evt {
            TrackVolumeChanged(e) if e.touched => TrackVolume { track: e.track },
            TrackPanChanged(e) if e.touched => {
                if let AvailablePanValue::Complete(new_value) = e.new_value {
                    figure_out_touched_pan_component(e.track, e.old_value, new_value)
                } else {
                    // Shouldn't result in this if touched.
                    return None;
                }
            }
            TrackSendVolumeChanged(e) if e.touched => TrackSendVolume { send: e.send },
            TrackSendPanChanged(e) if e.touched => TrackSendPan { send: e.send },
            TrackArmChanged(e) => TrackArm { track: e.track },
            TrackMuteChanged(e) if e.touched => TrackMute { track: e.track },
            TrackSoloChanged(e) => {
                // When we press the solo button of some track, REAPER actually sends many
                // change events, starting with the change event for the master track. This is
                // not cool for learning because we could only ever learn master-track solo,
                // which doesn't even make sense. So let's just filter it out.
                if e.track.is_master_track() {
                    return None;
                }
                TrackSolo { track: e.track }
            }
            TrackSelectedChanged(e) => TrackSelection {
                track: e.track,
                select_exclusively: false,
            },
            FxEnabledChanged(e) => FxEnable { fx: e.fx },
            FxParameterValueChanged(e) if e.touched => FxParameter { param: e.parameter },
            FxPresetChanged(e) => FxPreset { fx: e.fx },
            MasterTempoChanged(e) if e.touched => Tempo {
                // TODO-low In future this might come from a certain project
                project: Reaper::get().current_project(),
            },
            MasterPlayrateChanged(e) if e.touched => Playrate {
                // TODO-low In future this might come from a certain project
                project: Reaper::get().current_project(),
            },
            _ => return None,
        };
        Some(target)
    }

    pub fn touched() -> impl Event<Rc<ReaperTarget>> {
        use ReaperTarget::*;
        let reaper = Reaper::get();
        let csurf_rx = Global::control_surface_rx();
        let action_rx = Global::action_rx();
        observable::empty()
            .merge(
                csurf_rx
                    .fx_parameter_touched()
                    .map(move |param| FxParameter { param }.into()),
            )
            .merge(
                csurf_rx
                    .fx_enabled_changed()
                    .map(move |fx| FxEnable { fx }.into()),
            )
            .merge(
                csurf_rx
                    .fx_preset_changed()
                    .map(move |fx| FxPreset { fx }.into()),
            )
            .merge(
                csurf_rx
                    .track_volume_touched()
                    .map(move |track| TrackVolume { track }.into()),
            )
            .merge(csurf_rx.track_pan_touched().map(move |(track, old, new)| {
                figure_out_touched_pan_component(track, old, new).into()
            }))
            .merge(
                csurf_rx
                    .track_arm_changed()
                    .map(move |track| TrackArm { track }.into()),
            )
            .merge(csurf_rx.track_selected_changed().map(move |track| {
                TrackSelection {
                    track,
                    select_exclusively: false,
                }
                .into()
            }))
            .merge(
                csurf_rx
                    .track_mute_touched()
                    .map(move |track| TrackMute { track }.into()),
            )
            .merge(
                csurf_rx
                    .track_solo_changed()
                    // When we press the solo button of some track, REAPER actually sends many
                    // change events, starting with the change event for the master track. This is
                    // not cool for learning because we could only ever learn master-track solo,
                    // which doesn't even make sense. So let's just filter it out.
                    .filter(|track| !track.is_master_track())
                    .map(move |track| TrackSolo { track }.into()),
            )
            .merge(
                csurf_rx
                    .track_send_volume_touched()
                    .map(move |send| TrackSendVolume { send }.into()),
            )
            .merge(
                csurf_rx
                    .track_send_pan_touched()
                    .map(move |send| TrackSendPan { send }.into()),
            )
            .merge(
                action_rx
                    .action_invoked()
                    .map(move |action| determine_target_for_action((*action).clone()).into()),
            )
            .merge(
                csurf_rx
                    .master_tempo_touched()
                    // TODO-low In future this might come from a certain project
                    .map(move |_| {
                        Tempo {
                            project: reaper.current_project(),
                        }
                        .into()
                    }),
            )
            .merge(
                csurf_rx
                    .master_playrate_touched()
                    // TODO-low In future this might come from a certain project
                    .map(move |_| {
                        Playrate {
                            project: reaper.current_project(),
                        }
                        .into()
                    }),
            )
    }

    fn format_value_generic(&self, value: UnitValue) -> String {
        format!(
            "{} {}",
            self.format_value_without_unit(value),
            self.value_unit()
        )
    }

    /// Like `convert_unit_value_to_discrete_value()` but in the other direction.
    ///
    /// Used for parsing discrete values of discrete targets that can't do real parsing according to
    /// `can_parse_values()`.
    pub fn convert_discrete_value_to_unit_value(
        &self,
        value: u32,
    ) -> Result<UnitValue, &'static str> {
        if self.control_type().is_relative() {
            return (value as f64 / 63.0).try_into();
        }
        use ReaperTarget::*;
        let result = match self {
            FxPreset { fx } => {
                let index = if value == 0 { None } else { Some(value - 1) };
                fx_preset_unit_value(fx, index)
            }
            SelectedTrack { project } => {
                let index = if value == 0 { None } else { Some(value - 1) };
                selected_track_unit_value(*project, index)
            }
            FxParameter { param } => {
                let step_size = param.step_size().ok_or("not supported")?;
                (value as f64 * step_size).try_into()?
            }
            Action { .. }
            | TrackVolume { .. }
            | TrackSendVolume { .. }
            | TrackPan { .. }
            | TrackWidth { .. }
            | TrackArm { .. }
            | TrackSelection { .. }
            | TrackMute { .. }
            | TrackSolo { .. }
            | TrackSendPan { .. }
            | TrackSendMute { .. }
            | Tempo { .. }
            | Playrate { .. }
            | FxEnable { .. }
            | AllTrackFxEnable { .. }
            | LoadFxSnapshot { .. }
            | Transport { .. } => return Err("not supported"),
        };
        Ok(result)
    }

    fn parse_value_from_discrete_value(&self, text: &str) -> Result<UnitValue, &'static str> {
        self.convert_discrete_value_to_unit_value(text.parse().map_err(|_| "not a discrete value")?)
    }

    pub fn project(&self) -> Option<Project> {
        use ReaperTarget::*;
        let project = match self {
            Action { .. } | Transport { .. } => return None,
            FxParameter { param } => param.fx().project()?,
            TrackVolume { track }
            | TrackPan { track }
            | TrackWidth { track }
            | TrackArm { track }
            | TrackSelection { track, .. }
            | TrackMute { track }
            | TrackSolo { track }
            | AllTrackFxEnable { track } => track.project(),
            TrackSendPan { send } | TrackSendMute { send } | TrackSendVolume { send } => {
                send.source_track().project()
            }
            Tempo { project } | Playrate { project } | SelectedTrack { project } => *project,
            FxEnable { fx } | FxPreset { fx } | LoadFxSnapshot { fx, .. } => fx.project()?,
        };
        Some(project)
    }

    pub fn track(&self) -> Option<&Track> {
        use ReaperTarget::*;
        let track = match self {
            FxParameter { param } => param.fx().track()?,
            TrackVolume { track }
            | TrackPan { track }
            | TrackWidth { track }
            | TrackArm { track }
            | TrackSelection { track, .. }
            | TrackMute { track }
            | TrackSolo { track } => track,
            TrackSendPan { send } | TrackSendMute { send } | TrackSendVolume { send } => {
                send.source_track()
            }
            FxEnable { fx } | FxPreset { fx } | LoadFxSnapshot { fx, .. } => fx.track()?,
            AllTrackFxEnable { track } => track,
            Action { .. }
            | Tempo { .. }
            | Playrate { .. }
            | SelectedTrack { .. }
            | Transport { .. } => return None,
        };
        Some(track)
    }

    pub fn fx(&self) -> Option<&Fx> {
        use ReaperTarget::*;
        let fx = match self {
            FxParameter { param } => param.fx(),
            FxEnable { fx } | FxPreset { fx } | LoadFxSnapshot { fx, .. } => fx,
            Action { .. }
            | TrackVolume { .. }
            | TrackSendVolume { .. }
            | TrackPan { .. }
            | TrackWidth { .. }
            | TrackArm { .. }
            | TrackSelection { .. }
            | TrackMute { .. }
            | TrackSolo { .. }
            | TrackSendPan { .. }
            | TrackSendMute { .. }
            | Tempo { .. }
            | Playrate { .. }
            | SelectedTrack { .. }
            | AllTrackFxEnable { .. }
            | Transport { .. } => return None,
        };
        Some(fx)
    }

    pub fn send(&self) -> Option<&TrackSend> {
        use ReaperTarget::*;
        let send = match self {
            TrackSendPan { send } | TrackSendVolume { send } | TrackSendMute { send } => send,
            FxParameter { .. }
            | FxEnable { .. }
            | FxPreset { .. }
            | Action { .. }
            | TrackVolume { .. }
            | TrackPan { .. }
            | TrackWidth { .. }
            | TrackArm { .. }
            | TrackSelection { .. }
            | TrackMute { .. }
            | TrackSolo { .. }
            | Tempo { .. }
            | Playrate { .. }
            | SelectedTrack { .. }
            | AllTrackFxEnable { .. }
            | LoadFxSnapshot { .. }
            | Transport { .. } => return None,
        };
        Some(send)
    }

    pub fn supports_feedback(&self) -> bool {
        use ReaperTarget::*;
        match self {
            Action { .. }
            | FxParameter { .. }
            | TrackVolume { .. }
            | TrackSendVolume { .. }
            | TrackPan { .. }
            | TrackWidth { .. }
            | TrackArm { .. }
            | TrackSelection { .. }
            | TrackMute { .. }
            | TrackSolo { .. }
            | TrackSendPan { .. }
            | Tempo { .. }
            | Playrate { .. }
            | FxEnable { .. }
            | FxPreset { .. }
            | SelectedTrack { .. }
            | LoadFxSnapshot { .. }
            | Transport { .. } => true,
            AllTrackFxEnable { .. } | TrackSendMute { .. } => false,
        }
    }

    pub fn value_changed_from_additional_feedback_event(
        &self,
        evt: &AdditionalFeedbackEvent,
    ) -> (bool, Option<UnitValue>) {
        use AdditionalFeedbackEvent::*;
        use ReaperTarget::*;
        // TODO-high Get the correct values here!
        match self {
            Action { action, .. } => match evt {
                ActionInvoked(command_id) if *command_id == action.command_id() => (true, None),
                _ => (false, None),
            },
            LoadFxSnapshot { fx, .. } => match evt {
                FxSnapshotLoaded(f) if f == fx => (true, None),
                _ => (false, None),
            },
            FxParameter { param } => match evt {
                RealearnMonitoringFxParameterValueChanged(p) if p == param => (true, None),
                _ => (false, None),
            },
            _ => (false, None),
        }
    }

    /// Might return the new value if changed.
    pub fn value_changed_from_change_event(&self, evt: &ChangeEvent) -> (bool, Option<UnitValue>) {
        use ChangeEvent::*;
        use ReaperTarget::*;
        match self {
            FxParameter { param } => {
                match evt {
                    FxParameterValueChanged(e) if &e.parameter == param => (
                        true,
                        Some(fx_parameter_unit_value(&e.parameter, e.new_value))
                    ),
                    _ => (false, None)
                }
            }
            TrackVolume { track } => {
                match evt {
                    TrackVolumeChanged(e) if &e.track == track => (
                        true,
                        Some(volume_unit_value(Volume::from_reaper_value(e.new_value)))
                    ),
                    _ => (false, None)
                }
            }
            TrackSendVolume { send } => {
                match evt {
                    TrackSendVolumeChanged(e) if &e.send == send => (
                        true,
                        Some(volume_unit_value(Volume::from_reaper_value(e.new_value)))
                    ),
                    _ => (false, None)
                }
            }
            TrackPan { track } => {
                match evt {
                    TrackPanChanged(e) if &e.track == track => (
                        true,
                        {
                            let pan = match e.new_value {
                                AvailablePanValue::Complete(v) => v.main_pan(),
                                AvailablePanValue::Incomplete(pan) => pan
                            };
                            Some(pan_unit_value(Pan::from_reaper_value(pan)))
                        }
                    ),
                    _ => (false, None)
                }
            }
            TrackWidth { track } => {
                match evt {
                    TrackPanChanged(e) if &e.track == track => (
                        true,
                        match e.new_value {
                            AvailablePanValue::Complete(v) => if let Some(width) = v.width() {
                                Some(width_unit_value(Width::from_reaper_value(width)))
                            } else {
                                None
                            }
                            AvailablePanValue::Incomplete(_) => None
                        }
                    ),
                    _ => (false, None)
                }
            }
            TrackArm { track } => {
                match evt {
                    TrackArmChanged(e) if &e.track == track => (
                        true,
                        Some(track_arm_unit_value(e.new_value))
                    ),
                    _ => (false, None)
                }
            }
            TrackSelection { track, .. } => {
                match evt {
                    TrackSelectedChanged(e) if &e.track == track => (
                        true,
                        Some(track_selected_unit_value(e.new_value))
                    ),
                    _ => (false, None)
                }
            }
            TrackMute { track } => {
                match evt {
                    TrackMuteChanged(e) if &e.track == track => (
                        true,
                        Some(mute_unit_value(e.new_value))
                    ),
                    _ => (false, None)
                }
            }
            TrackSolo { track } => {
                match evt {
                    TrackSoloChanged(e) if &e.track == track => (
                        true,
                        Some(track_solo_unit_value(e.new_value))
                    ),
                    _ => (false, None)
                }
            }
            TrackSendPan { send } => {
                match evt {
                    TrackSendPanChanged(e) if &e.send == send => (
                        true,
                        Some(pan_unit_value(Pan::from_reaper_value(e.new_value)))
                    ),
                    _ => (false, None)
                }
            }
            Tempo { .. } => match evt {
                MasterTempoChanged(e) => (
                    true,
                    Some(tempo_unit_value(reaper_high::Tempo::from_bpm(e.new_value)))
                ),
                _ => (false, None)
            },
            Playrate { .. } => match evt {
                MasterPlayrateChanged(e) => (
                    true,
                    Some(playrate_unit_value(PlayRate::from_playback_speed_factor(e.new_value)))
                ),
                _ => (false, None)
            },
            FxEnable { fx } => {
                match evt {
                    FxEnabledChanged(e) if &e.fx == fx => (
                        true,
                        Some(fx_enable_unit_value(e.new_value))
                    ),
                    _ => (false, None)
                }
            }
            FxPreset { fx } => {
                match evt {
                    FxPresetChanged(e) if &e.fx == fx => (true, None),
                    _ => (false, None)
                }
            }
            SelectedTrack { project } => {
                match evt {
                    TrackSelectedChanged(e) if &e.track.project() == project => (
                        true,
                        Some(track_selected_unit_value(e.new_value))
                    ),
                    _ => (false, None)
                }
            }
            Transport { action, .. } => {
                match *action {
                    TransportAction::PlayStop | TransportAction::PlayPause => match evt {
                        PlayStateChanged(e) => (
                            true,
                            Some(transport_is_enabled_unit_value(e.new_value.is_playing))
                        ),
                        _ => (false, None)
                    }
                    TransportAction::Record => match evt {
                        PlayStateChanged(e) => (
                            true,
                            Some(transport_is_enabled_unit_value(e.new_value.is_recording))
                        ),
                        _ => (false, None)
                    }
                    TransportAction::Repeat => match evt {
                        RepeatStateChanged(e) => (
                            true,
                            Some(transport_is_enabled_unit_value(e.new_value))
                        ),
                        _ => (false, None)
                    }
                }
            }
            // Handled from non-control-surface callbacks.
            Action { .. }
            | LoadFxSnapshot { .. }
            // No value change notification available.
            | TrackSendMute { .. }
            | AllTrackFxEnable { .. }
             => (false, None),
        }
    }

    pub fn value_changed(&self) -> BoxedUnitEvent {
        use ReaperTarget::*;
        let csurf_rx = Global::control_surface_rx();
        let action_rx = Global::action_rx();
        match self {
            Action {
                action,
                invocation_type: _,
                ..
            } => {
                let action = action.clone();
                // TODO-medium It's not cool that reaper-rs exposes some events as Rc<T>
                //  and some not
                action_rx
                    .action_invoked()
                    .filter(move |a| a.as_ref() == &action)
                    .map_to(())
                    .box_it()
            }
            FxParameter { param } => {
                let param = param.clone();
                csurf_rx
                    .fx_parameter_value_changed()
                    .filter(move |p| p == &param)
                    .map_to(())
                    .box_it()
            }
            TrackVolume { track } => {
                let track = track.clone();
                csurf_rx
                    .track_volume_changed()
                    .filter(move |t| t == &track)
                    .map_to(())
                    .box_it()
            }
            TrackSendVolume { send } => {
                let send = send.clone();
                csurf_rx
                    .track_send_volume_changed()
                    .filter(move |s| s == &send)
                    .map_to(())
                    .box_it()
            }
            TrackPan { track } | TrackWidth { track } => {
                let track = track.clone();
                csurf_rx
                    .track_pan_changed()
                    .filter(move |t| t == &track)
                    .map_to(())
                    .box_it()
            }
            TrackArm { track } => {
                let track = track.clone();
                csurf_rx
                    .track_arm_changed()
                    .filter(move |t| t == &track)
                    .map_to(())
                    .box_it()
            }
            TrackSelection { track, .. } => {
                let track = track.clone();
                csurf_rx
                    .track_selected_changed()
                    .filter(move |t| t == &track)
                    .map_to(())
                    .box_it()
            }
            TrackMute { track } => {
                let track = track.clone();
                csurf_rx
                    .track_mute_changed()
                    .filter(move |t| t == &track)
                    .map_to(())
                    .box_it()
            }
            TrackSolo { track } => {
                let track = track.clone();
                csurf_rx
                    .track_solo_changed()
                    .filter(move |t| t == &track)
                    .map_to(())
                    .box_it()
            }
            TrackSendPan { send } => {
                let send = send.clone();
                csurf_rx
                    .track_send_pan_changed()
                    .filter(move |s| s == &send)
                    .map_to(())
                    .box_it()
            }
            Tempo { .. } => csurf_rx.master_tempo_changed().map_to(()).box_it(),
            Playrate { .. } => csurf_rx.master_playrate_changed().map_to(()).box_it(),
            FxEnable { fx } => {
                let fx = fx.clone();
                csurf_rx
                    .fx_enabled_changed()
                    .filter(move |f| f == &fx)
                    .map_to(())
                    .box_it()
            }
            FxPreset { fx } => {
                let fx = fx.clone();
                csurf_rx
                    .fx_preset_changed()
                    .filter(move |f| f == &fx)
                    .map_to(())
                    .box_it()
            }
            LoadFxSnapshot { fx, .. } => {
                let fx = fx.clone();
                DomainGlobal::target_context()
                    .borrow()
                    .fx_snapshot_loaded()
                    .filter(move |f| f == &fx)
                    .map_to(())
                    .box_it()
            }
            SelectedTrack { project } => {
                let project = *project;
                csurf_rx
                    .track_selected_changed()
                    .filter(move |t| t.project() == project)
                    .map_to(())
                    .box_it()
            }
            Transport { action, .. } => {
                if *action == TransportAction::Repeat {
                    csurf_rx.repeat_state_changed().box_it()
                } else {
                    csurf_rx.play_state_changed().box_it()
                }
            }
            AllTrackFxEnable { .. } | TrackSendMute { .. } => observable::never().box_it(),
        }
    }
}

impl Target for ReaperTarget {
    fn current_value(&self) -> Option<UnitValue> {
        use ReaperTarget::*;
        let result = match self {
            Action { action, .. } => {
                if let Some(state) = action.is_on() {
                    // Toggle action: Return toggle state as 0 or 1.
                    convert_bool_to_unit_value(state)
                } else {
                    // Non-toggle action. Try to return current absolute value if this is a
                    // MIDI CC/mousewheel action.
                    if let Some(value) = action.normalized_value() {
                        UnitValue::new(value)
                    } else {
                        UnitValue::MIN
                    }
                }
            }
            FxParameter { param } => {
                fx_parameter_unit_value(param, param.reaper_normalized_value())
            }
            TrackVolume { track } => volume_unit_value(track.volume()),
            TrackSendVolume { send } => volume_unit_value(send.volume()),
            TrackPan { track } => pan_unit_value(track.pan()),
            TrackWidth { track } => width_unit_value(track.width()),
            TrackArm { track } => track_arm_unit_value(track.is_armed(false)),
            TrackSelection { track, .. } => track_selected_unit_value(track.is_selected()),
            TrackMute { track } => mute_unit_value(track.is_muted()),
            TrackSolo { track } => track_solo_unit_value(track.is_solo()),
            TrackSendPan { send } => pan_unit_value(send.pan()),
            TrackSendMute { send } => mute_unit_value(send.is_muted()),
            Tempo { project } => tempo_unit_value(project.tempo()),
            Playrate { project } => playrate_unit_value(project.play_rate()),
            FxEnable { fx } => fx_enable_unit_value(fx.is_enabled()),
            FxPreset { fx } => fx_preset_unit_value(fx, fx.preset_index().ok()?),
            SelectedTrack { project } => selected_track_unit_value(
                *project,
                project
                    .first_selected_track(MasterTrackBehavior::ExcludeMasterTrack)
                    .and_then(|t| t.index()),
            ),
            AllTrackFxEnable { track } => all_track_fx_enable_unit_value(track.fx_is_enabled()),
            Transport { project, action } => {
                use TransportAction::*;
                match action {
                    PlayStop | PlayPause => transport_is_enabled_unit_value(project.is_playing()),
                    Record => transport_is_enabled_unit_value(project.is_recording()),
                    Repeat => transport_is_enabled_unit_value(project.repeat_is_enabled()),
                }
            }
            LoadFxSnapshot { fx, chunk_hash, .. } => {
                let is_loaded = DomainGlobal::target_context()
                    .borrow()
                    .current_fx_snapshot_chunk_hash(fx)
                    == Some(*chunk_hash);
                convert_bool_to_unit_value(is_loaded)
            }
        };
        Some(result)
    }

    fn control_type(&self) -> ControlType {
        use ReaperTarget::*;
        match self {
            Action {
                invocation_type,
                action,
                ..
            } => {
                use ActionInvocationType::*;
                match *invocation_type {
                    Trigger => ControlType::AbsoluteTrigger,
                    Absolute => match action.character() {
                        ActionCharacter::Toggle => ControlType::AbsoluteSwitch,
                        ActionCharacter::Trigger => ControlType::AbsoluteContinuous,
                    },
                    Relative => ControlType::Relative,
                }
            }
            FxParameter { param } => {
                use GetParameterStepSizesResult::*;
                match param.step_sizes() {
                    None => ControlType::AbsoluteContinuous,
                    Some(Normal {
                        normal_step,
                        small_step,
                        ..
                    }) => {
                        // The reported step sizes relate to the reported value range, which is not
                        // always the unit interval! Easy to test with JS
                        // FX.
                        let range = param.value_range();
                        // We are primarily interested in the smallest step size that makes sense.
                        // We can always create multiples of it.
                        let span = (range.max_val - range.min_val).abs();
                        if span == 0.0 {
                            return ControlType::AbsoluteContinuous;
                        }
                        let pref_step_size = small_step.unwrap_or(normal_step);
                        let step_size = pref_step_size / span;
                        ControlType::AbsoluteDiscrete {
                            atomic_step_size: UnitValue::new(step_size),
                        }
                    }
                    Some(Toggle) => ControlType::AbsoluteSwitch,
                }
            }
            Tempo { .. } => ControlType::AbsoluteContinuousRoundable {
                rounding_step_size: UnitValue::new(1.0 / bpm_span()),
            },
            Playrate { .. } => ControlType::AbsoluteContinuousRoundable {
                rounding_step_size: UnitValue::new(1.0 / (playback_speed_factor_span() * 100.0)),
            },
            // `+ 1` because "<no preset>" is also a possible value.
            FxPreset { fx } => {
                let preset_count = fx.preset_count().unwrap_or(0);
                ControlType::AbsoluteDiscrete {
                    atomic_step_size: convert_count_to_step_size(preset_count + 1),
                }
            }
            // `+ 1` because "<Master track>" is also a possible value.
            SelectedTrack { project } => ControlType::AbsoluteDiscrete {
                atomic_step_size: convert_count_to_step_size(project.track_count() + 1),
            },
            TrackArm { .. }
            | TrackSelection { .. }
            | TrackMute { .. }
            | TrackSendMute { .. }
            | FxEnable { .. }
            | AllTrackFxEnable { .. }
            | Transport { .. }
            | TrackSolo { .. } => ControlType::AbsoluteSwitch,
            TrackVolume { .. }
            | TrackSendVolume { .. }
            | TrackPan { .. }
            | TrackWidth { .. }
            | TrackSendPan { .. } => ControlType::AbsoluteContinuous,
            LoadFxSnapshot { .. } => ControlType::AbsoluteTrigger,
        }
    }
}

/// Converts a number of possible values to a step size.
fn convert_count_to_step_size(n: u32) -> UnitValue {
    // Dividing 1.0 by n would divide the unit interval (0..=1) into n same-sized
    // sub intervals, which means we would have n + 1 possible values. We want to
    // represent just n values, so we need n - 1 same-sized sub intervals.
    if n == 0 || n == 1 {
        return UnitValue::MAX;
    }
    UnitValue::new(1.0 / (n - 1) as f64)
}

fn format_value_as_playback_speed_factor_without_unit(value: UnitValue) -> String {
    let play_rate = PlayRate::from_normalized_value(NormalizedPlayRate::new(value.get()));
    format_playback_speed(play_rate.playback_speed_factor().get())
}

fn format_playback_speed(speed: f64) -> String {
    format!("{:.4}", speed)
}

fn format_step_size_as_playback_speed_factor_without_unit(value: UnitValue) -> String {
    // 0.0 => 0.0x
    // 1.0 => 3.75x
    let speed_increment = value.get() * playback_speed_factor_span();
    format_playback_speed(speed_increment)
}

fn format_value_as_bpm_without_unit(value: UnitValue) -> String {
    let tempo = Tempo::from_normalized_value(value.get());
    format_bpm(tempo.bpm().get())
}

fn format_step_size_as_bpm_without_unit(value: UnitValue) -> String {
    // 0.0 => 0.0 bpm
    // 1.0 => 959.0 bpm
    let bpm_increment = value.get() * bpm_span();
    format_bpm(bpm_increment)
}

// Should be 959.0
fn bpm_span() -> f64 {
    Bpm::MAX.get() - Bpm::MIN.get()
}

fn format_bpm(bpm: f64) -> String {
    format!("{:.4}", bpm)
}

fn format_value_as_db_without_unit(value: UnitValue) -> String {
    let db = Volume::from_soft_normalized_value(value.get()).db();
    if db == Db::MINUS_INF {
        "-inf".to_string()
    } else {
        format!("{:.2}", db.get())
    }
}

fn format_value_as_db(value: UnitValue) -> String {
    Volume::from_soft_normalized_value(value.get()).to_string()
}

fn format_value_as_pan(value: UnitValue) -> String {
    Pan::from_normalized_value(value.get()).to_string()
}

fn format_value_as_on_off(value: UnitValue) -> &'static str {
    if value.is_zero() { "Off" } else { "On" }
}

fn convert_bool_to_unit_value(on: bool) -> UnitValue {
    if on { UnitValue::MAX } else { UnitValue::MIN }
}

fn convert_unit_value_to_preset_index(fx: &Fx, value: UnitValue) -> Option<u32> {
    convert_unit_to_discrete_value_with_none(value, fx.preset_count().ok()?)
}

fn convert_unit_value_to_track_index(project: Project, value: UnitValue) -> Option<u32> {
    convert_unit_to_discrete_value_with_none(value, project.track_count())
}

fn convert_unit_to_discrete_value_with_none(value: UnitValue, count: u32) -> Option<u32> {
    // Example: <no preset> + 4 presets
    if value.is_zero() {
        // 0.00 => <no preset>
        None
    } else {
        // 0.25 => 0
        // 0.50 => 1
        // 0.75 => 2
        // 1.00 => 3

        // Example: value = 0.75
        let step_size = 1.0 / count as f64; // 0.25
        let zero_based_value = (value.get() - step_size).max(0.0); // 0.5
        Some((zero_based_value * count as f64).round() as u32) // 2
    }
}

fn selected_track_unit_value(project: Project, index: Option<u32>) -> UnitValue {
    convert_discrete_to_unit_value_with_none(index, project.track_count())
}

fn fx_preset_unit_value(fx: &Fx, index: Option<u32>) -> UnitValue {
    convert_discrete_to_unit_value_with_none(index, fx.preset_count().unwrap_or(0))
}

fn convert_discrete_to_unit_value_with_none(value: Option<u32>, count: u32) -> UnitValue {
    // Example: <no preset> + 4 presets
    match value {
        // <no preset> => 0.00
        None => UnitValue::MIN,
        // 0 => 0.25
        // 1 => 0.50
        // 2 => 0.75
        // 3 => 1.00
        Some(i) => {
            if count == 0 {
                return UnitValue::MIN;
            }
            // Example: i = 2
            let zero_based_value = i as f64 / count as f64; // 0.5
            let step_size = 1.0 / count as f64; // 0.25
            let value = (zero_based_value + step_size).min(1.0); // 0.75
            UnitValue::new(value)
        }
    }
}

fn parse_value_from_db(text: &str) -> Result<UnitValue, &'static str> {
    let decimal: f64 = text.parse().map_err(|_| "not a decimal value")?;
    let db: Db = decimal.try_into().map_err(|_| "not in dB range")?;
    Volume::from_db(db).soft_normalized_value().try_into()
}

fn parse_value_from_pan(text: &str) -> Result<UnitValue, &'static str> {
    let pan: Pan = text.parse()?;
    pan.normalized_value().try_into()
}

fn parse_value_from_playback_speed_factor(text: &str) -> Result<UnitValue, &'static str> {
    let decimal: f64 = text.parse().map_err(|_| "not a decimal value")?;
    let factor: PlaybackSpeedFactor = decimal.try_into().map_err(|_| "not in play rate range")?;
    PlayRate::from_playback_speed_factor(factor)
        .normalized_value()
        .get()
        .try_into()
}

fn parse_step_size_from_playback_speed_factor(text: &str) -> Result<UnitValue, &'static str> {
    // 0.0x => 0.0
    // 3.75x => 1.0
    let decimal: f64 = text.parse().map_err(|_| "not a decimal value")?;
    let span = playback_speed_factor_span();
    if decimal < 0.0 || decimal > span {
        return Err("not in playback speed factor increment range");
    }
    Ok(UnitValue::new(decimal / span))
}

/// Should be 3.75
fn playback_speed_factor_span() -> f64 {
    PlaybackSpeedFactor::MAX.get() - PlaybackSpeedFactor::MIN.get()
}

fn parse_value_from_bpm(text: &str) -> Result<UnitValue, &'static str> {
    let decimal: f64 = text.parse().map_err(|_| "not a decimal value")?;
    let bpm: Bpm = decimal.try_into().map_err(|_| "not in BPM range")?;
    Tempo::from_bpm(bpm).normalized_value().try_into()
}

fn parse_step_size_from_bpm(text: &str) -> Result<UnitValue, &'static str> {
    // 0.0 bpm => 0.0
    // 959.0 bpm => 1.0
    let decimal: f64 = text.parse().map_err(|_| "not a decimal value")?;
    let span = bpm_span();
    if decimal < 0.0 || decimal > span {
        return Err("not in BPM increment range");
    }
    Ok(UnitValue::new(decimal / span))
}

/// How to invoke an action target
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    Serialize_repr,
    Deserialize_repr,
    IntoEnumIterator,
    TryFromPrimitive,
    IntoPrimitive,
    Display,
)]
#[repr(usize)]
pub enum ActionInvocationType {
    #[display(fmt = "Trigger")]
    Trigger = 0,
    #[display(fmt = "Absolute")]
    Absolute = 1,
    #[display(fmt = "Relative")]
    Relative = 2,
}

impl Default for ActionInvocationType {
    fn default() -> Self {
        ActionInvocationType::Absolute
    }
}

#[derive(
    Copy,
    Clone,
    Eq,
    PartialEq,
    Debug,
    Serialize,
    Deserialize,
    IntoEnumIterator,
    TryFromPrimitive,
    IntoPrimitive,
    Display,
)]
#[repr(usize)]
pub enum TransportAction {
    #[serde(rename = "playStop")]
    #[display(fmt = "Play/stop")]
    PlayStop,
    #[serde(rename = "playPause")]
    #[display(fmt = "Play/pause")]
    PlayPause,
    #[serde(rename = "record")]
    #[display(fmt = "Record")]
    Record,
    #[serde(rename = "repeat")]
    #[display(fmt = "Repeat")]
    Repeat,
}

impl Default for TransportAction {
    fn default() -> Self {
        TransportAction::PlayStop
    }
}

fn determine_target_for_action(action: Action) -> ReaperTarget {
    let project = Reaper::get().current_project();
    match action.command_id().get() {
        // Play button | stop button
        1007 | 1016 => ReaperTarget::Transport {
            project,
            action: TransportAction::PlayStop,
        },
        // Pause button
        1008 => ReaperTarget::Transport {
            project,
            action: TransportAction::PlayPause,
        },
        // Record button
        1013 => ReaperTarget::Transport {
            project,
            action: TransportAction::Record,
        },
        // Repeat button
        1068 => ReaperTarget::Transport {
            project,
            action: TransportAction::Repeat,
        },
        _ => ReaperTarget::Action {
            action,
            invocation_type: ActionInvocationType::Trigger,
            project,
        },
    }
}

trait PanExt {
    /// Returns the pan value. In case of dual-pan, returns the left pan value.
    fn main_pan(self) -> ReaperPanValue;
    fn width(self) -> Option<ReaperWidthValue>;
}

impl PanExt for reaper_medium::Pan {
    /// Returns the pan value. In case of dual-pan, returns the left pan value.
    fn main_pan(self) -> ReaperPanValue {
        use reaper_medium::Pan::*;
        match self {
            BalanceV1(p) => p,
            BalanceV4(p) => p,
            StereoPan { pan, .. } => pan,
            DualPan { left, .. } => left,
        }
    }

    fn width(self) -> Option<ReaperWidthValue> {
        if let reaper_medium::Pan::StereoPan { width, .. } = self {
            Some(width)
        } else {
            None
        }
    }
}

fn figure_out_touched_pan_component(
    track: Track,
    old: reaper_medium::Pan,
    new: reaper_medium::Pan,
) -> ReaperTarget {
    if old.width() != new.width() {
        ReaperTarget::TrackWidth { track }
    } else {
        ReaperTarget::TrackPan { track }
    }
}

fn fx_parameter_unit_value(param: &FxParameter, value: ReaperNormalizedFxParamValue) -> UnitValue {
    let v = value.get();
    if !UnitValue::is_valid(v) {
        // Either the FX reports a wrong value range (e.g. TAL Flanger Sync Speed)
        // or the value range exceeded a "normal" range (e.g. ReaPitch Wet). We can't
        // know. In future, we might offer further customization possibilities here.
        // For now, we just report it as 0.0 or 1.0 and log a warning.
        warn!(
            Reaper::get().logger(),
            "FX parameter reported normalized value {:?} which is not in unit interval: {:?}",
            v,
            param
        );
        return UnitValue::new_clamped(v);
    }
    UnitValue::new(v)
}

fn volume_unit_value(volume: Volume) -> UnitValue {
    // The soft-normalized value can be > 1.0, e.g. when we have a volume of 12 dB and then
    // lower the volume fader limit to a lower value. In that case we just report the
    // highest possible value ... not much else we can do.
    UnitValue::new_clamped(volume.soft_normalized_value())
}

fn pan_unit_value(pan: Pan) -> UnitValue {
    UnitValue::new(pan.normalized_value())
}

fn width_unit_value(width: Width) -> UnitValue {
    UnitValue::new(width.normalized_value())
}

fn track_arm_unit_value(is_armed: bool) -> UnitValue {
    convert_bool_to_unit_value(is_armed)
}

fn track_selected_unit_value(is_selected: bool) -> UnitValue {
    convert_bool_to_unit_value(is_selected)
}

fn mute_unit_value(is_mute: bool) -> UnitValue {
    convert_bool_to_unit_value(is_mute)
}

fn track_solo_unit_value(is_solo: bool) -> UnitValue {
    convert_bool_to_unit_value(is_solo)
}

fn tempo_unit_value(tempo: Tempo) -> UnitValue {
    UnitValue::new(tempo.normalized_value())
}

fn playrate_unit_value(playrate: PlayRate) -> UnitValue {
    UnitValue::new(playrate.normalized_value().get())
}

fn fx_enable_unit_value(is_enabled: bool) -> UnitValue {
    convert_bool_to_unit_value(is_enabled)
}

fn all_track_fx_enable_unit_value(is_enabled: bool) -> UnitValue {
    convert_bool_to_unit_value(is_enabled)
}

fn transport_is_enabled_unit_value(is_enabled: bool) -> UnitValue {
    convert_bool_to_unit_value(is_enabled)
}
