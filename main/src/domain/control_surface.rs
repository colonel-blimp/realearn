use crate::core::Global;
use crate::domain::{
    ActivationChange, BackboneState, CompoundMappingSource, DeviceControlInput,
    DeviceFeedbackOutput, DomainEventHandler, EelTransformation, FeedbackOutput, InstanceId,
    LifecycleMidiData, MainProcessor, OscDeviceId, OscInputDevice, RealSource,
    RealTimeCompoundMappingTarget, RealTimeMapping, ReaperTarget, SharedRealTimeProcessor,
    SourceFeedbackValue, TouchedParameterType,
};
use crossbeam_channel::Receiver;
use helgoboss_learn::{OscSource, RawMidiEvent};
use reaper_high::{
    ChangeDetectionMiddleware, ControlSurfaceEvent, ControlSurfaceMiddleware, FutureMiddleware, Fx,
    FxParameter, MainTaskMiddleware, MeterMiddleware, Project, Reaper,
};
use reaper_rx::ControlSurfaceRxMiddleware;
use rosc::{OscMessage, OscPacket};

use reaper_medium::{
    CommandId, ExtSupportsExtendedTouchArgs, GetTouchStateArgs, MediaTrack, PositionInSeconds,
    ReaProject, ReaperNormalizedFxParamValue,
};
use rxrust::prelude::*;
use slog::debug;
use smallvec::SmallVec;
use std::collections::HashMap;

type LearnSourceSender = async_channel::Sender<(OscDeviceId, OscSource)>;

const CONTROL_SURFACE_MAIN_TASK_BULK_SIZE: usize = 10;
const CONTROL_SURFACE_SERVER_TASK_BULK_SIZE: usize = 10;
const ADDITIONAL_FEEDBACK_EVENT_BULK_SIZE: usize = 30;
const INSTANCE_ORCHESTRATION_EVENT_BULK_SIZE: usize = 30;
const OSC_INCOMING_BULK_SIZE: usize = 32;
const GARBAGE_BULK_SIZE: usize = 100;

#[derive(Debug)]
pub struct RealearnControlSurfaceMiddleware<EH: DomainEventHandler> {
    logger: slog::Logger,
    change_detection_middleware: ChangeDetectionMiddleware,
    rx_middleware: ControlSurfaceRxMiddleware,
    main_processors: Vec<MainProcessor<EH>>,
    main_task_receiver: Receiver<RealearnControlSurfaceMainTask<EH>>,
    server_task_receiver: Receiver<RealearnControlSurfaceServerTask>,
    additional_feedback_event_receiver: Receiver<AdditionalFeedbackEvent>,
    instance_orchestration_event_receiver: Receiver<InstanceOrchestrationEvent>,
    meter_middleware: MeterMiddleware,
    main_task_middleware: MainTaskMiddleware,
    future_middleware: FutureMiddleware,
    counter: u64,
    full_beats: HashMap<ReaProject, u32>,
    metrics_enabled: bool,
    state: State,
    osc_input_devices: Vec<OscInputDevice>,
    garbage_receiver: crossbeam_channel::Receiver<Garbage>,
}

#[derive(Debug)]
pub enum Garbage {
    RawMidiEvent(Box<RawMidiEvent>),
    RealTimeProcessor(SharedRealTimeProcessor),
    LifecycleMidiData(LifecycleMidiData),
    ResolvedTarget(Option<RealTimeCompoundMappingTarget>),
    EelTransformation(Option<EelTransformation>),
    MappingSource(CompoundMappingSource),
    RealTimeMappings(Vec<RealTimeMapping>),
    BoxedRealTimeMapping(Box<Option<RealTimeMapping>>),
    ActivationChanges(Vec<ActivationChange>),
}

#[derive(Debug)]
enum State {
    Normal,
    LearningSource(LearnSourceSender),
    LearningTarget(async_channel::Sender<ReaperTarget>),
}

pub enum RealearnControlSurfaceMainTask<EH: DomainEventHandler> {
    // Removing a main processor is done synchronously by temporarily regaining ownership of the
    // control surface from REAPER.
    AddMainProcessor(MainProcessor<EH>),
    LogDebugInfo,
    StartLearningTargets(async_channel::Sender<ReaperTarget>),
    StartLearningSources(LearnSourceSender),
    StopLearning,
    SendAllFeedback,
}

/// Not all events in REAPER are communicated via a control surface, e.g. action invocations.
#[derive(Debug)]
pub enum AdditionalFeedbackEvent {
    ActionInvoked(ActionInvokedEvent),
    FxSnapshotLoaded(FxSnapshotLoadedEvent),
    /// Work around REAPER's inability to notify about parameter changes in
    /// monitoring FX by simulating the notification ourselves.
    /// Then parameter learning and feedback works at least for
    /// ReaLearn monitoring FX instances, which is especially
    /// useful for conditional activation.
    RealearnMonitoringFxParameterValueChanged(RealearnMonitoringFxParameterValueChangedEvent),
    ParameterAutomationTouchStateChanged(ParameterAutomationTouchStateChangedEvent),
    BeatChanged(BeatChangedEvent),
}

#[derive(Debug)]
pub enum InstanceOrchestrationEvent {
    /// Sent by a ReaLearn instance X if it releases control over a source.
    ///
    /// This enables other instances to take over control of that source before X finally "switches
    /// off lights".
    SourceReleased(SourceReleasedEvent),
    /// Whenever something about instance's device usage changes (either input or output or both
    /// potentially change).
    IoUpdated(IoUpdatedEvent),
}

/// Communicates changes in which input and output device a ReaLearn instance uses or used.
#[derive(Debug)]
pub struct IoUpdatedEvent {
    pub instance_id: InstanceId,
    pub control_input: Option<DeviceControlInput>,
    pub control_input_used: bool,
    pub feedback_output: Option<DeviceFeedbackOutput>,
    pub feedback_output_used: bool,
    pub feedback_output_usage_might_have_changed: bool,
}

#[derive(Debug)]
pub struct SourceReleasedEvent {
    pub instance_id: InstanceId,
    pub feedback_output: FeedbackOutput,
    pub feedback_value: SourceFeedbackValue,
}

#[derive(Debug)]
pub struct BeatChangedEvent {
    pub project: Project,
    pub new_value: PositionInSeconds,
}

#[derive(Debug)]
pub struct ActionInvokedEvent {
    pub command_id: CommandId,
}

#[derive(Debug)]
pub struct FxSnapshotLoadedEvent {
    pub fx: Fx,
}

#[derive(Debug)]
pub struct RealearnMonitoringFxParameterValueChangedEvent {
    pub parameter: FxParameter,
    pub new_value: ReaperNormalizedFxParamValue,
}

#[derive(Debug)]
pub struct ParameterAutomationTouchStateChangedEvent {
    pub track: MediaTrack,
    pub parameter_type: TouchedParameterType,
    pub new_value: bool,
}

pub enum RealearnControlSurfaceServerTask {
    ProvidePrometheusMetrics(tokio::sync::oneshot::Sender<String>),
}

impl<EH: DomainEventHandler> RealearnControlSurfaceMiddleware<EH> {
    pub fn new(
        parent_logger: &slog::Logger,
        main_task_receiver: Receiver<RealearnControlSurfaceMainTask<EH>>,
        server_task_receiver: Receiver<RealearnControlSurfaceServerTask>,
        additional_feedback_event_receiver: Receiver<AdditionalFeedbackEvent>,
        instance_orchestration_event_receiver: Receiver<InstanceOrchestrationEvent>,
        garbage_receiver: crossbeam_channel::Receiver<Garbage>,
        metrics_enabled: bool,
    ) -> Self {
        let logger = parent_logger.new(slog::o!("struct" => "RealearnControlSurfaceMiddleware"));
        Self {
            logger: logger.clone(),
            change_detection_middleware: ChangeDetectionMiddleware::new(),
            rx_middleware: ControlSurfaceRxMiddleware::new(Global::control_surface_rx().clone()),
            main_processors: Default::default(),
            main_task_receiver,
            server_task_receiver,
            additional_feedback_event_receiver,
            instance_orchestration_event_receiver,
            meter_middleware: MeterMiddleware::new(logger.clone()),
            main_task_middleware: MainTaskMiddleware::new(
                logger.clone(),
                Global::get().task_sender(),
                Global::get().task_receiver(),
            ),
            future_middleware: FutureMiddleware::new(
                logger.clone(),
                Global::get().executor(),
                Global::get().local_executor(),
            ),
            counter: 0,
            full_beats: Default::default(),
            metrics_enabled,
            state: State::Normal,
            osc_input_devices: vec![],
            garbage_receiver,
        }
    }

    pub fn remove_main_processor(&mut self, id: &InstanceId) {
        self.main_processors.retain(|p| p.instance_id() != id);
    }

    pub fn set_osc_input_devices(&mut self, devs: Vec<OscInputDevice>) {
        self.osc_input_devices = devs;
    }

    pub fn clear_osc_input_devices(&mut self) {
        self.osc_input_devices.clear();
    }

    /// Called when waking up ReaLearn (first instance appears again or the first time).
    pub fn wake_up(&self) {
        self.change_detection_middleware.reset(|e| {
            for m in &self.main_processors {
                m.process_control_surface_change_event(&e);
            }
            self.rx_middleware.handle_change(e);
        });
        // We don't want to execute tasks which accumulated during the "downtime" of Reaper.
        // So we just consume all without executing them.
        self.main_task_middleware.reset();
        self.future_middleware.reset();
    }

    fn run_internal(&mut self) {
        // Run middlewares
        self.main_task_middleware.run();
        self.future_middleware.run();
        self.rx_middleware.run();
        // Process main tasks
        for t in self
            .main_task_receiver
            .try_iter()
            .take(CONTROL_SURFACE_MAIN_TASK_BULK_SIZE)
        {
            use RealearnControlSurfaceMainTask::*;
            match t {
                AddMainProcessor(p) => {
                    self.main_processors.push(p);
                }
                LogDebugInfo => {
                    self.log_debug_info();
                    self.meter_middleware.log_metrics();
                }
                StartLearningTargets(sender) => {
                    self.state = State::LearningTarget(sender);
                }
                StopLearning => {
                    self.state = State::Normal;
                }
                StartLearningSources(sender) => {
                    self.state = State::LearningSource(sender);
                }
                SendAllFeedback => {
                    for m in &self.main_processors {
                        m.send_all_feedback();
                    }
                }
            }
        }
        // Process server tasks
        for t in self
            .server_task_receiver
            .try_iter()
            .take(CONTROL_SURFACE_SERVER_TASK_BULK_SIZE)
        {
            use RealearnControlSurfaceServerTask::*;
            match t {
                ProvidePrometheusMetrics(sender) => {
                    let text = serde_prometheus::to_string(
                        self.meter_middleware.metrics(),
                        Some("realearn"),
                        HashMap::new(),
                    )
                    .unwrap();
                    let _ = sender.send(text);
                }
            }
        }
        // Process incoming additional feedback
        for event in self
            .additional_feedback_event_receiver
            .try_iter()
            .take(ADDITIONAL_FEEDBACK_EVENT_BULK_SIZE)
        {
            if let AdditionalFeedbackEvent::RealearnMonitoringFxParameterValueChanged(e) = &event {
                let rx = Global::control_surface_rx();
                rx.fx_parameter_value_changed
                    .borrow_mut()
                    .next(e.parameter.clone());
                rx.fx_parameter_touched
                    .borrow_mut()
                    .next(e.parameter.clone());
            }
            for p in &mut self.main_processors {
                p.process_additional_feedback_event(&event)
            }
        }
        // Process instance orchestration events
        for event in self
            .instance_orchestration_event_receiver
            .try_iter()
            .take(INSTANCE_ORCHESTRATION_EVENT_BULK_SIZE)
        {
            use InstanceOrchestrationEvent::*;
            match event {
                SourceReleased(e) => {
                    debug!(self.logger, "Source of instance {} released", e.instance_id);
                    let other_instance_took_over =
                        if let Some(source) = RealSource::from_feedback_value(&e.feedback_value) {
                            // We also allow the instance to take over which released the source in
                            // the first place! Simply because in the meanwhile, this instance
                            // could have found a new usage for it! E.g. likely to happen with
                            // preset changes.
                            self.main_processors
                                .iter()
                                .any(|p| p.maybe_takeover_source(&source))
                        } else {
                            false
                        };
                    if !other_instance_took_over {
                        if let Some(p) = self
                            .main_processors
                            .iter()
                            .find(|p| p.instance_id() == &e.instance_id)
                        {
                            // Finally safe to switch off lights!
                            p.finally_switch_off_source(e.feedback_output, e.feedback_value);
                        }
                    }
                }
                IoUpdated(e) => {
                    let backbone_state = BackboneState::get();
                    let feedback_dev_usage_changed = backbone_state.update_io_usage(
                        &e.instance_id,
                        if e.control_input_used {
                            e.control_input
                        } else {
                            None
                        },
                        if e.feedback_output_used {
                            e.feedback_output
                        } else {
                            None
                        },
                    );
                    if feedback_dev_usage_changed
                        && backbone_state.lives_on_upper_floor(&e.instance_id)
                    {
                        debug!(
                            self.logger,
                            "Upper-floor instance {} {} feedback output",
                            e.instance_id,
                            if e.feedback_output_used {
                                "claimed"
                            } else {
                                "released"
                            }
                        );
                        if let Some(feedback_output) = e.feedback_output {
                            // Give lower-floor instances the chance to cancel or reactivate.
                            self.main_processors
                                .iter()
                                .filter(|p| p.instance_id() != &e.instance_id)
                                .for_each(|p| {
                                    p.handle_change_of_some_upper_floor_instance(feedback_output)
                                });
                        }
                    }
                }
            }
        }
        // Emit beats as feedback events
        for project in Reaper::get().projects() {
            let reference_pos = if project.is_playing() {
                project.play_position_latency_compensated()
            } else {
                project.edit_cursor_position()
            };
            if self.record_possible_beat_change(project, reference_pos) {
                let event = AdditionalFeedbackEvent::BeatChanged(BeatChangedEvent {
                    project,
                    new_value: reference_pos,
                });
                for p in &mut self.main_processors {
                    p.process_additional_feedback_event(&event);
                }
            }
        }
        // OSC
        self.process_incoming_osc_messages();
        // Main processors
        match &self.state {
            State::Normal => {
                for p in &mut self.main_processors {
                    p.run_all();
                }
            }
            State::LearningSource(_) | State::LearningTarget(_) => {
                for p in &mut self.main_processors {
                    p.run_essential();
                }
            }
        }
        // Metrics
        if self.metrics_enabled {
            // Roughly every 10 seconds
            if self.counter == 30 * 10 {
                self.meter_middleware.warn_about_critical_metrics();
                self.counter = 0;
            } else {
                self.counter += 1;
            }
        }
        // Garbage drop
        for garbage in self.garbage_receiver.try_iter().take(GARBAGE_BULK_SIZE) {
            let _ = garbage;
        }
    }

    fn log_debug_info(&self) {
        // Summary
        let msg = format!(
            "\n\
            # Backbone control surface\n\
            \n\
            - Garbage count: {} \n\
            ",
            self.garbage_receiver.len(),
        );
        Reaper::get().show_console_msg(msg);
    }

    fn process_incoming_osc_messages(&mut self) {
        pub type PacketVec = SmallVec<[OscPacket; OSC_INCOMING_BULK_SIZE]>;
        let packets_by_device: SmallVec<[(OscDeviceId, PacketVec); OSC_INCOMING_BULK_SIZE]> = self
            .osc_input_devices
            .iter_mut()
            .map(|dev| {
                (
                    *dev.id(),
                    dev.poll_multiple(OSC_INCOMING_BULK_SIZE).collect(),
                )
            })
            .collect();
        for (dev_id, packets) in packets_by_device {
            match &self.state {
                State::Normal => {
                    for proc in &mut self.main_processors {
                        if proc.receives_osc_from(&dev_id) {
                            for packet in &packets {
                                proc.process_incoming_osc_packet(packet);
                            }
                        }
                    }
                }
                State::LearningSource(sender) => {
                    for packet in packets {
                        process_incoming_osc_packet_for_learning(dev_id, sender, packet)
                    }
                }
                State::LearningTarget(_) => {}
            }
        }
    }

    fn handle_event_internal(&self, event: ControlSurfaceEvent) -> bool {
        // We always need to forward to the change detection middleware even if we are in
        // a mode in which the detected change event doesn't matter!
        self.change_detection_middleware.process(event, |e| {
            match &self.state {
                State::Normal => {
                    // This is for feedback processing. No Rx!
                    for m in &self.main_processors {
                        m.process_control_surface_change_event(&e);
                    }
                    // The rest is only for upper layers (e.g. UI), not for processing.
                    self.rx_middleware.handle_change(e.clone());
                    if let Some(target) = ReaperTarget::touched_from_change_event(e) {
                        // TODO-medium Now we have the necessary framework (AdditionalFeedbackEvent)
                        //  to also support action, FX snapshot and ReaLearn monitoring FX parameter
                        //  touching for "Last touched" target and global learning (see
                        //  LearningTarget state)! Connect the dots!
                        BackboneState::get().set_last_touched_target(target);
                        for p in &self.main_processors {
                            p.notify_target_touched();
                        }
                    }
                }
                State::LearningTarget(sender) => {
                    // At some point we want the Rx stuff out of the domain layer. This is one step
                    // in this direction.
                    if let Some(target) = ReaperTarget::touched_from_change_event(e) {
                        let _ = sender.try_send(target);
                    }
                }
                State::LearningSource(_) => {}
            }
        })
    }

    fn record_possible_beat_change(
        &mut self,
        project: Project,
        reference_pos: PositionInSeconds,
    ) -> bool {
        let beat_info = project.beat_info_at(reference_pos);
        let new_full_beats = beat_info.full_beats.get() as _;
        let full_beats = self.full_beats.entry(project.raw()).or_default();
        let beat_changed = new_full_beats != *full_beats;
        *full_beats = new_full_beats;
        beat_changed
    }
}

impl<EH: DomainEventHandler> ControlSurfaceMiddleware for RealearnControlSurfaceMiddleware<EH> {
    fn run(&mut self) {
        if self.metrics_enabled {
            let elapsed = MeterMiddleware::measure(|| {
                self.run_internal();
            });
            self.meter_middleware.record_run(elapsed);
        } else {
            self.run_internal();
        }
    }

    fn handle_event(&self, event: ControlSurfaceEvent) -> bool {
        if self.metrics_enabled {
            let elapsed = MeterMiddleware::measure(|| {
                self.handle_event_internal(event);
            });
            self.meter_middleware.record_event(event, elapsed)
        } else {
            self.handle_event_internal(event)
        }
    }

    fn get_touch_state(&self, args: GetTouchStateArgs) -> bool {
        if let Ok(domain_type) = TouchedParameterType::try_from_reaper(args.parameter_type) {
            BackboneState::target_context()
                .borrow()
                .automation_parameter_is_touched(args.track, domain_type)
        } else {
            false
        }
    }

    fn ext_supports_extended_touch(&self, _: ExtSupportsExtendedTouchArgs) -> i32 {
        1
    }
}

fn process_incoming_osc_packet_for_learning(
    dev_id: OscDeviceId,
    sender: &LearnSourceSender,
    packet: OscPacket,
) {
    match packet {
        OscPacket::Message(msg) => process_incoming_osc_message_for_learning(dev_id, sender, msg),
        OscPacket::Bundle(bundle) => {
            for p in bundle.content.into_iter() {
                process_incoming_osc_packet_for_learning(dev_id, sender, p);
            }
        }
    }
}

fn process_incoming_osc_message_for_learning(
    dev_id: OscDeviceId,
    sender: &LearnSourceSender,
    msg: OscMessage,
) {
    let source = OscSource::from_source_value(msg, Some(0));
    let _ = sender.try_send((dev_id, source));
}

impl<EH: DomainEventHandler> Drop for RealearnControlSurfaceMiddleware<EH> {
    fn drop(&mut self) {
        for garbage in self.garbage_receiver.try_iter() {
            let _ = garbage;
        }
    }
}

#[derive(Clone, Debug)]
pub struct GarbageBin {
    sender: crossbeam_channel::Sender<Garbage>,
}
impl GarbageBin {
    pub fn new(sender: crossbeam_channel::Sender<Garbage>) -> Self {
        assert!(
            sender.capacity().is_some(),
            "garbage bin sender channel must be bounded!"
        );
        Self { sender }
    }

    pub fn dispose(&self, garbage: Garbage) {
        self.sender.try_send(garbage).unwrap();
    }

    pub fn dispose_real_time_mapping(&self, m: RealTimeMapping) {
        // Dispose bits that contain heap-allocated stuff. Do it separately to not let the garbage
        // enum size get too large.
        self.dispose(Garbage::LifecycleMidiData(m.lifecycle_midi_data));
        self.dispose(Garbage::ResolvedTarget(m.resolved_target));
        self.dispose(Garbage::EelTransformation(
            m.core.mode.control_transformation,
        ));
        self.dispose(Garbage::EelTransformation(
            m.core.mode.feedback_transformation,
        ));
        self.dispose(Garbage::MappingSource(m.core.source));
    }
}
