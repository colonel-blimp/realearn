use crate::application::ModeModel;
use crate::core::default_util::{is_default, is_unit_value_one, unit_value_one};
use crate::infrastructure::data::MigrationDescriptor;
use crate::infrastructure::plugin::App;
use helgoboss_learn::{
    AbsoluteMode, ButtonUsage, EncoderUsage, FireMode, Interval, OutOfRangeBehavior,
    SoftSymmetricUnitValue, TakeoverMode, UnitValue,
};
use serde::{Deserialize, Serialize};
use slog::debug;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeModelData {
    #[serde(default, skip_serializing_if = "is_default")]
    r#type: AbsoluteMode,
    #[serde(default, skip_serializing_if = "is_default")]
    min_source_value: UnitValue,
    #[serde(default = "unit_value_one", skip_serializing_if = "is_unit_value_one")]
    max_source_value: UnitValue,
    #[serde(default, skip_serializing_if = "is_default")]
    min_target_value: UnitValue,
    #[serde(default = "unit_value_one", skip_serializing_if = "is_unit_value_one")]
    max_target_value: UnitValue,
    #[serde(default, skip_serializing_if = "is_default")]
    min_target_jump: UnitValue,
    #[serde(default = "unit_value_one", skip_serializing_if = "is_unit_value_one")]
    max_target_jump: UnitValue,
    #[serde(
        default = "default_step_size",
        skip_serializing_if = "is_default_step_size"
    )]
    min_step_size: SoftSymmetricUnitValue,
    #[serde(
        default = "default_step_size",
        skip_serializing_if = "is_default_step_size"
    )]
    max_step_size: SoftSymmetricUnitValue,
    #[serde(default, skip_serializing_if = "is_default")]
    min_press_millis: u64,
    #[serde(default, skip_serializing_if = "is_default")]
    max_press_millis: u64,
    #[serde(default, skip_serializing_if = "is_default")]
    turbo_rate: u64,
    #[serde(default, skip_serializing_if = "is_default")]
    eel_control_transformation: String,
    #[serde(default, skip_serializing_if = "is_default")]
    eel_feedback_transformation: String,
    #[serde(default, skip_serializing_if = "is_default")]
    reverse_is_enabled: bool,
    // Serialization skipped because this is deprecated in favor of out_of_range_behavior
    // since ReaLearn v1.11.0.
    #[serde(default, skip_serializing)]
    ignore_out_of_range_source_values_is_enabled: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    out_of_range_behavior: OutOfRangeBehavior,
    #[serde(default, skip_serializing_if = "is_default")]
    fire_mode: FireMode,
    #[serde(default, skip_serializing_if = "is_default")]
    round_target_value: bool,
    // Serialization skipped because this is deprecated in favor of takeover_mode
    // since ReaLearn v2.8.0-pre3.
    #[serde(default, skip_serializing)]
    scale_mode_enabled: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    takeover_mode: TakeoverMode,
    #[serde(default, skip_serializing_if = "is_default")]
    button_usage: ButtonUsage,
    #[serde(default, skip_serializing_if = "is_default")]
    encoder_usage: EncoderUsage,
    #[serde(default, skip_serializing_if = "is_default")]
    rotate_is_enabled: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    make_absolute_enabled: bool,
}

fn default_step_size() -> SoftSymmetricUnitValue {
    SoftSymmetricUnitValue::new(0.01)
}

fn is_default_step_size(v: &SoftSymmetricUnitValue) -> bool {
    *v == default_step_size()
}

impl ModeModelData {
    pub fn from_model(model: &ModeModel) -> Self {
        Self {
            r#type: model.r#type.get(),
            min_source_value: model.source_value_interval.get_ref().min_val(),
            max_source_value: model.source_value_interval.get_ref().max_val(),
            min_target_value: model.target_value_interval.get_ref().min_val(),
            max_target_value: model.target_value_interval.get_ref().max_val(),
            min_target_jump: model.jump_interval.get_ref().min_val(),
            max_target_jump: model.jump_interval.get_ref().max_val(),
            min_step_size: model.step_interval.get_ref().min_val(),
            max_step_size: model.step_interval.get_ref().max_val(),
            min_press_millis: model
                .press_duration_interval
                .get_ref()
                .min_val()
                .as_millis() as _,
            max_press_millis: model
                .press_duration_interval
                .get_ref()
                .max_val()
                .as_millis() as _,
            turbo_rate: model.turbo_rate.get().as_millis() as _,
            eel_control_transformation: model.eel_control_transformation.get_ref().clone(),
            eel_feedback_transformation: model.eel_feedback_transformation.get_ref().clone(),
            reverse_is_enabled: model.reverse.get(),
            // Not used anymore since ReaLearn v1.11.0
            ignore_out_of_range_source_values_is_enabled: false,
            out_of_range_behavior: model.out_of_range_behavior.get(),
            fire_mode: model.fire_mode.get(),
            round_target_value: model.round_target_value.get(),
            // Not used anymore since ReaLearn v2.8.0-pre3
            scale_mode_enabled: false,
            takeover_mode: model.takeover_mode.get(),
            button_usage: model.button_usage.get(),
            encoder_usage: model.encoder_usage.get(),
            rotate_is_enabled: model.rotate.get(),
            make_absolute_enabled: model.make_absolute.get(),
        }
    }

    pub fn apply_to_model(&self, model: &mut ModeModel) {
        self.apply_to_model_flexible(model, &MigrationDescriptor::default(), "", true);
    }

    pub fn apply_to_model_flexible(
        &self,
        model: &mut ModeModel,
        migration_descriptor: &MigrationDescriptor,
        mapping_name: &str,
        with_notification: bool,
    ) {
        model
            .r#type
            .set_with_optional_notification(self.r#type, with_notification);
        model.source_value_interval.set_with_optional_notification(
            Interval::new(self.min_source_value, self.max_source_value),
            with_notification,
        );
        {
            let saved_target_interval = Interval::new(self.min_target_value, self.max_target_value);
            let actual_target_interval = if migration_descriptor.target_interval_transformation_117
                && self.reverse_is_enabled
                && self.r#type == AbsoluteMode::Normal
            {
                debug!(
                    App::logger(),
                    "Migration: Inverting target interval of mapping {} in order to not break existing behavior because of #117",
                    mapping_name
                );
                saved_target_interval.inverse()
            } else {
                saved_target_interval
            };
            model
                .target_value_interval
                .set_with_optional_notification(actual_target_interval, with_notification);
        }
        model.step_interval.set_with_optional_notification(
            Interval::new(self.min_step_size, self.max_step_size),
            with_notification,
        );
        model
            .press_duration_interval
            .set_with_optional_notification(
                Interval::new(
                    Duration::from_millis(self.min_press_millis),
                    Duration::from_millis(self.max_press_millis),
                ),
                with_notification,
            );
        model.turbo_rate.set_with_optional_notification(
            Duration::from_millis(self.turbo_rate),
            with_notification,
        );
        model.jump_interval.set_with_optional_notification(
            Interval::new(self.min_target_jump, self.max_target_jump),
            with_notification,
        );
        model
            .eel_control_transformation
            .set_with_optional_notification(
                self.eel_control_transformation.clone(),
                with_notification,
            );
        model
            .eel_feedback_transformation
            .set_with_optional_notification(
                self.eel_feedback_transformation.clone(),
                with_notification,
            );
        model
            .reverse
            .set_with_optional_notification(self.reverse_is_enabled, with_notification);
        let actual_out_of_range_behavior = if self.ignore_out_of_range_source_values_is_enabled {
            // Data saved with ReaLearn version < 1.11.0
            OutOfRangeBehavior::Ignore
        } else {
            self.out_of_range_behavior
        };
        model
            .fire_mode
            .set_with_optional_notification(self.fire_mode, with_notification);
        model
            .out_of_range_behavior
            .set_with_optional_notification(actual_out_of_range_behavior, with_notification);
        model
            .round_target_value
            .set_with_optional_notification(self.round_target_value, with_notification);
        let takeover_mode = if self.scale_mode_enabled {
            // ReaLearn < 2.8.0-pre3 used this flag instead of the enum.
            TakeoverMode::LongTimeNoSee
        } else {
            self.takeover_mode
        };
        model
            .takeover_mode
            .set_with_optional_notification(takeover_mode, with_notification);
        model
            .button_usage
            .set_with_optional_notification(self.button_usage, with_notification);
        model
            .encoder_usage
            .set_with_optional_notification(self.encoder_usage, with_notification);
        model
            .rotate
            .set_with_optional_notification(self.rotate_is_enabled, with_notification);
        model
            .make_absolute
            .set_with_optional_notification(self.make_absolute_enabled, with_notification);
    }
}
