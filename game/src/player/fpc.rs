//! Port of `FirstPersonController.cs` + `SlopeSlide.cs` + `Slope.cs` + `CustomAddForce.cs`
//! + `CameraShakeConstrains.cs`. Values are from the PlayerIK prefab, not the C# defaults.
//!
//! Frame order matches Unity: input callbacks -> (previous Move's collision callbacks) ->
//! Slope/SlopeSlide/CustomAddForce Update -> FirstPersonController.Update -> Move (Rapier KCC in
//! PostUpdate) -> DOTween update -> CameraShakeConstrains.
//!
//! Not ported (outside scope): networking, voice chat, moving platforms, teleporters,
//! glass breaking, stat counters. Lens distortion / color grading values are tracked but the
//! post-process effect itself is TODO.

use crate::input::PlayerInput;
use crate::physics::{self, LADDER_LAYER, LAND_LAYER, UnityTag};
use crate::settings::Settings;
use crate::tween::{Ease, Tweens};
use crate::unity::*;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

// ---------------------------------------------------------------------------------------------
// Parameters (serialized fields, PlayerIK prefab values)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct FpcParams {
    pub walk_speed: f32,
    pub sprint_speed: f32,
    pub crouch_speed: f32,
    pub air_control: f32,
    pub air_speed: f32,
    pub sprint_air_speed: f32,
    pub global_acceleration: f32,
    pub global_deceleration: f32,
    pub walk_acceleration: f32,
    pub sprint_acceleration: f32,
    pub crouch_acceleration: f32,
    pub air_acceleration: f32,
    pub sprint_air_acceleration: f32,
    pub upper_look_limit: f32,
    pub lower_look_limit: f32,
    pub kill_shock_wave_strength: f32,
    pub saturation_speed: f32,
    pub lean_speed: f32,
    pub lean_limit: f32,
    pub jump_force: f32,
    pub jump_slope_force: f32,
    pub jump_slope_decel: f32,
    pub gravity: f32,
    pub crouch_gravity: f32,
    pub jump_gravity: f32,
    pub ladder_detection_length: f32,
    pub ladder_speed: f32,
    pub vault_minimum_angle: f32,
    pub crouch_height: f32,
    pub slide_height: f32,
    pub stand_height: f32,
    pub crouching_speed: f32,
    pub walk_slide_impulsion: f32,
    pub walk_slide_duration: f32,
    pub sprint_slide_impulsion: f32,
    pub sprint_slide_duration: f32,
    pub slide_reset_time: f32,
    pub land_recover_speed: f32,
    pub land_bob_amount: f32,
    pub land_bob_duration: f32,
    pub land_bob_ease: Ease,
    pub fall_shake_height: f32,
    pub fall_shake_force: Vec3,
    pub fall_shake_duration: f32,
    pub fall_shake_vibrato: i32,
    pub fall_shake_elasticity: f32,
    pub high_fall_shake_force: Vec3,
    pub high_fall_shake_duration: f32,
    pub high_fall_shake_recover_speed: f32,
    pub high_fall_shake_height: f32,
    pub fall_audio_lerp_speed: f32,
    pub fall_audio_lerp_out_speed: f32,
    pub lens_distortion_when_falling: f32,
    pub lens_distortion_lerp_speed: f32,
    pub lens_distortion_lerp_out_speed: f32,
    pub zoom_speed: f32,
    pub zoom_fov: f32,
    pub zoom_ease: Ease,
    pub run_ease_speed: f32,
    pub run_fov: f32,
    pub slide_ease: Ease,
    pub slide_ease_speed: f32,
    pub slide_fov: f32,
    pub run_slide_ease: Ease,
    pub run_slide_ease_speed: f32,
    pub run_slide_fov: f32,
    pub run_ease: Ease,
    pub tilt_amount: f32,
    pub tilt_speed: f32,
    pub slide_tilt_amount: f32,
    pub rotation_amount: f32,
    pub max_rotation_amount: f32,
    pub smooth_rotation: f32,
    pub base_step_speed: f32,
    pub crouch_step_multiplier: f32,
    pub sprint_step_multiplier: f32,
    pub ladder_step: f32,
    #[allow(dead_code)] // FirstPersonController data, unused by the single-player map
    pub walk_step_volume: f32,
    pub slope_force: f32,
    pub slope_force_ray_length: f32,
    pub force_speed: f32,
    #[allow(dead_code)] // FirstPersonController data, unused by the single-player map
    pub moving_platform_eject_force: f32,
    /// Camera.fieldOfView at Start (used for the dist-to-run/slide FOV offsets).
    pub prefab_camera_fov: f32,
    // CharacterController
    pub cc_radius: f32,
    pub cc_slope_limit: f32,
    pub cc_step_offset: f32,
}

impl Default for FpcParams {
    fn default() -> Self {
        Self {
            walk_speed: 5.5,
            sprint_speed: 8.5,
            crouch_speed: 4.0,
            air_control: 0.2,
            air_speed: 6.0,
            sprint_air_speed: 9.0,
            global_acceleration: 15.0,
            global_deceleration: 5.0,
            walk_acceleration: 30.0,
            sprint_acceleration: 20.0,
            crouch_acceleration: 2.0,
            air_acceleration: 6.0,
            sprint_air_acceleration: 6.0,
            upper_look_limit: 90.0,
            lower_look_limit: 90.0,
            kill_shock_wave_strength: -55.0,
            saturation_speed: 2.5,
            lean_speed: 8.0,
            lean_limit: 30.0,
            jump_force: 8.0,
            jump_slope_force: 12.0,
            jump_slope_decel: 4.0,
            gravity: 30.0,
            crouch_gravity: 30.0,
            jump_gravity: 30.0,
            ladder_detection_length: 1.0,
            ladder_speed: 6.0,
            vault_minimum_angle: 80.0,
            crouch_height: 1.25,
            slide_height: 0.8,
            stand_height: 2.0,
            crouching_speed: 20.0,
            walk_slide_impulsion: 0.5,
            walk_slide_duration: 25.0,
            sprint_slide_impulsion: 0.2,
            sprint_slide_duration: 40.0,
            slide_reset_time: 1.0,
            land_recover_speed: 2.9,
            land_bob_amount: 0.65,
            land_bob_duration: 0.1,
            land_bob_ease: Ease::Linear,
            fall_shake_height: 2.0,
            fall_shake_force: Vec3::new(-2.0, 0.0, -3.0),
            fall_shake_duration: 0.2,
            fall_shake_vibrato: 11,
            fall_shake_elasticity: 1.19,
            high_fall_shake_force: Vec3::new(-30.0, 0.0, -7.0),
            high_fall_shake_duration: 0.15,
            high_fall_shake_recover_speed: 3.0,
            high_fall_shake_height: 10.0,
            fall_audio_lerp_speed: 0.6,
            fall_audio_lerp_out_speed: 12.0,
            lens_distortion_when_falling: -50.0,
            lens_distortion_lerp_speed: 3.0,
            lens_distortion_lerp_out_speed: 2.3,
            zoom_speed: 0.2,
            zoom_fov: 45.0,
            zoom_ease: Ease::Linear,
            run_ease_speed: 0.3,
            run_fov: 83.0,
            slide_ease: Ease::Linear,
            slide_ease_speed: 0.6,
            slide_fov: 80.0,
            run_slide_ease: Ease::Linear,
            run_slide_ease_speed: 0.6,
            run_slide_fov: 83.0,
            run_ease: Ease::Linear,
            tilt_amount: 3.5,
            tilt_speed: 5.0,
            slide_tilt_amount: 10.0,
            rotation_amount: 0.0,
            max_rotation_amount: 5.0,
            smooth_rotation: 12.0,
            base_step_speed: 0.5,
            crouch_step_multiplier: 1.5,
            sprint_step_multiplier: 0.6,
            ladder_step: 0.3,
            walk_step_volume: 0.5,
            slope_force: 400.0,
            slope_force_ray_length: 1.0,
            force_speed: 30.0,
            moving_platform_eject_force: 50.0,
            prefab_camera_fov: 68.0,
            cc_radius: 0.4,
            cc_slope_limit: 65.0,
            cc_step_offset: 0.3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SlopeSlideParams {
    pub concrete_speed: f32,
    pub ice_speed: f32,
    pub min_walk_ice_slide_speed: f32,
    pub max_walk_ice_slide_speed: f32,
    pub sand_speed: f32,
    pub concrete_fade_out_time: f32,
    pub ice_fade_out_time: f32,
    pub sand_fade_out_time: f32,
    pub concrete_acceleration: f32,
    pub ice_acceleration: f32,
    pub sand_acceleration: f32,
    pub concrete_deceleration: f32,
    pub ice_deceleration: f32,
    pub sand_deceleration: f32,
    pub first_clamp: f32,
    pub second_clamp: f32,
    pub min_slope_angle: f32,
    pub walk_first_clamp: f32,
    pub walk_second_clamp: f32,
    pub steep_slope_slide_deceleration: f32,
}

impl Default for SlopeSlideParams {
    fn default() -> Self {
        Self {
            concrete_speed: 20.0,
            ice_speed: 26.0,
            min_walk_ice_slide_speed: 4.0,
            max_walk_ice_slide_speed: 20.0,
            sand_speed: 20.0,
            concrete_fade_out_time: 0.8,
            ice_fade_out_time: 5.0,
            sand_fade_out_time: 0.6,
            concrete_acceleration: 1.0,
            ice_acceleration: 2.0,
            sand_acceleration: 8.0,
            concrete_deceleration: 3.0,
            ice_deceleration: 1.0,
            sand_deceleration: 20.0,
            first_clamp: 10.0,
            second_clamp: 30.0,
            min_slope_angle: 13.0,
            walk_first_clamp: 0.0,
            walk_second_clamp: 50.0,
            steep_slope_slide_deceleration: 10.0,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------------------------

/// CameraHolder local y after FirstPersonController.Awake: prefab 2.5 + `offset.y` (PlayerIK FPC offset (0, -0.35, 0)).
pub const CAMERA_HOLDER_Y: f32 = 2.5 - 0.35;

/// Camera rig parts (PlayerIK: CameraLeanPivot / LandBobPivot / CameraHolder / Camera).
#[derive(Component)]
pub struct LeanCamera;
#[derive(Component)]
pub struct LandBobPivot;
#[derive(Component)]
pub struct PlayerCameraHolder;
#[derive(Component)]
pub struct PlayerCamera;

/// Sounds FirstPersonController plays (PlaySoundServer indices + extra one-shots).
#[derive(Message, Clone, Debug)]
pub enum FpcSound {
    /// PlaySoundServer(n) with the ground tag at the time.
    Indexed {
        index: u8,
        ground_tag: String,
    },
    WallJump {
        wall_tag: String,
    },
    Vault {
        surface_tag: String,
    },
    CeilingHit,
    Taunt(u8),
}

#[derive(Component)]
pub struct Fpc {
    pub p: FpcParams,
    pub ss: SlopeSlideParams,
    // state flags
    pub can_move: bool,
    pub is_sprinting: bool,
    pub is_walking: bool,
    pub is_crouching: bool,
    pub is_grounded: bool,
    pub safe_grounded: bool,
    pub is_sliding: bool,
    pub is_leaning: bool,
    pub is_touching_anything: bool,
    /// `isTouchingAnything` as left by the last CharacterController.Move (read by the DamageZone trigger, fx.rs).
    pub touched_last_move: bool,
    /// PlayerPickup.hasObjectInLeftHand (disables aiming).
    pub has_left_hand: bool,
    // multipliers set by held weapons
    pub movement_factor: f32,
    pub jump_factor: f32,
    pub wall_jump_factor: f32,
    pub max_wall_jumps: i32,
    pub gravity_multiplier: f32,
    // movement
    pub move_direction: Vec3,
    pub move_added: Vec3,
    pub object_collision_move_direction: Vec3,
    pub ground_normal: Vec3,
    pub player_speed: f32,
    pub speed_factor: f32,
    pub current_input: Vec2,
    pub current_input_raw: Vec2,
    vertical_input: f32,
    horizontal_input: f32,
    pub look_speed_x: f32,
    pub look_speed_y: f32,
    pub look_speed_aim: f32,
    pub look_speed_aim_no_scope: f32,
    pub is_scope_aiming: bool,
    pub rotation_x: f32,
    rotation_z: f32,
    mouse_input: Vec2,
    /// `PauseManager.gamepad` (x14 look speed, gamepad-only EndSlide).
    pub gamepad: bool,
    prejump: bool,
    walk_slide: bool,
    sprint_slide: bool,
    pub is_zooming: bool,
    pub is_aiming: bool,
    pub default_fov: f32,
    run_fov: f32,
    slide_fov: f32,
    run_slide_fov: f32,
    dist_to_run_fov: f32,
    dist_to_slide_fov: f32,
    dist_to_run_slide_fov: f32,
    pub fov: f32,
    /// Live `playerCamera.DOFieldOfView(fov, speed).SetEase(e)` tweens: (start captured at first update, elapsed,
    /// end, ease, duration). A new one is created every frame the FOV differs from its goal.
    fov_tweens: Vec<(Option<f32>, f32, f32, Ease, f32)>,
    hit_point_normal: Vec3,
    pub wall_point_normal: Vec3,
    /// `IsSliding` (on too-steep ground), distinct from `isSliding` (crouch slide).
    pub is_sliding_steep: bool,
    pub can_wall_jump: bool,
    currentwalltag: String,
    pub wall_jumps_count: i32,
    hit_diagonal_ceiling: bool,
    pub wall_slide_lean: f32,
    pub land_bool: bool,
    land_timer: f32,
    slide_end: bool,
    jump_slope: bool,
    on_ladder: bool,
    aftervaultjump_timer: f32,
    slide_timer: f32,
    set_speed_timer: f32,
    slide_reset_timer: f32,
    slide_cancel_timer: f32,
    slide_time: f32,
    /// SpawnVFX(slideDust) positions queued by handle_crouch, emitted by `slide_dust_fx`.
    pub slide_dust: Vec<Vec3>,
    coyote_timer: f32,
    pub fall_distance: f32,
    crouch_press: bool,
    pub jumped: bool,
    all_jumped: bool,
    pub ground_sphere_cast: bool,
    taunt_timer: f32,
    pub slope_slide_jumped: bool,
    // raycasts
    pub shortfront_ray: bool,
    pub vaul_ray_down: bool,
    pub vaul_ray_up: bool,
    pub vault_ray_up_short: bool,
    steep_slide_forward: bool,
    steep_slide_backward: bool,
    pub down_ray: bool,
    anim_down_ray: bool,
    slide_down_ray: bool,
    up_sphere: bool,
    ladder_ray: bool,
    ladder_tag: String,
    flymode: bool,
    // settings mirrors
    pub sprint_toggle: bool,
    pub aim_toggle: bool,
    pub lean_toggle: bool,
    pub crouch_toggle: bool,
    pub reverse_sprint_bind: bool,
    pub invert_x: bool,
    pub invert_y: bool,
    func_sprint: bool,
    func_sprint_trigger: bool,
    is_leaning_left: bool,
    is_leaning_right: bool,
    func_lean_right_trigger: bool,
    func_lean_left_trigger: bool,
    temp_set_speed: f32,
    dir_forward: Vec3,
    dir_right: Vec3,
    crouch_exit: bool,
    prejump_land: bool,
    /// networkAnimator.SetTrigger("Jump") pending for the third-person body (consumed by local_body_anim).
    pub anim_jump: bool,
    activate_fall_audio: bool,
    is_slide_sprinting: bool,
    ground_tag: String,
    /// JumpClipObservers' own probe: tag of the collider 3 m below transform.position on the default raycast layers
    /// ("Untagged" if it has none), "" on a miss (-> jumpClip).
    jump_tag: String,
    right_head_cast: bool,
    left_head_cast: bool,
    // AddForce
    pub adding_force: bool,
    pub force_added: Vec3,
    pub force: Vec3,
    pub force_factor: f32,
    pub initforce_factor: f32,
    pub tempforce_speed: f32,
    pub deceleration: f32,
    pub custom_force_final: Vec3,
    // BForce
    bforcefinal: Vec3,
    bdirection: Vec3,
    bfactor: f32,
    bdecel: f32,
    bstop: bool,
    bground_stop: bool,
    baircontrol: bool,
    btimer: f32,
    pub vault: bool,
    vault_activate: bool,
    deactivate_vault_timer: f32,
    rotate_back_after_vault_timer: f32,
    footstep_timer: f32,
    ladder_step_timer: f32,
    // CharacterController state
    pub cc_height: f32,
    pub cc_center: Vec3,
    pub cc_skin: f32,
    pub cc_grounded: bool,
    /// Normal of the steepest-up ground contact of the last Move (None = no ground contact).
    pub cc_ground_contact: Option<Vec3>,
    /// Last Move's contacts steeper than slopeLimit but facing up (distinct normals): the non-walkable slide.
    pub cc_steep_contacts: Vec<Vec3>,
    pub cc_velocity: Vec3,
    // audio/post state (consumed by audio/post modules)
    pub fall_audio_volume: f32,
    pub fall_audio_playing: bool,
    pub slide_audio_volume: f32,
    pub slide_audio_playing: bool,
    pub slide_clip: u8,
    pub lens_distortion: f32,
    pub saturation: f32,
    pub high_fall_vfx_alpha: f32,
    /// Third-person torso/legs shown while sliding (ActiveTorso).
    pub show_body_parts: bool,
    body_parts_pending: bool,
    // Slope.cs
    pub slope_uphill: bool,
    // CustomAddForce.cs
    impact: Vec3,
    // SlopeSlide.cs
    pub slope_slide_move: Vec3,
    pub steep_slope_slide_move: Vec3,
    pub is_crouch_slope_sliding: bool,
    pub slope_slide_trigger: bool,
    slope_slide_trigger2: bool,
    pub on_ice: bool,
    pub on_super_ice: bool,
    on_sand: bool,
    on_concrete: bool,
    sprint_slide_trigger: bool,
    ss_local_slope_slide_trigger: bool,
    ss_local_slope_slide_stop_trigger: bool,
    ss_slide_cancel_timer: f32,
    ss_init_sound_volume: f32,
    ss_crouch_out_trigger: bool,
    ss_sprint_in_trigger: bool,
    ss_sprint_late_trigger: bool,
    ss_current_speed: f32,
    ss_current_acceleration: f32,
    ss_current_deceleration: f32,
    ss_stopped: bool,
    pub ss_audio_volume: f32,
    pub ss_audio_playing: bool,
    // CameraShakeConstrains
    pub cam_rotate_back: bool,
    /// Pending Weapon.WeaponAnimation tween on FPArms (see `crate::weapons::FpArmsAnim`).
    pub fparms_anim: Option<crate::weapons::FpArmsAnim>,
    pub cam_base_speed: f32,
    // previous Move result
    last_motion: Vec3,
    /// Frame values of the Update that issued the last Move (its tail runs at the start of the next frame here):
    /// dt, move input, run, lean left / right.
    last_dt: f32,
    last_input: (Vec2, f32, f32, f32),
    pub safe_death_falling: bool,
}

impl Default for Fpc {
    fn default() -> Self {
        let p = FpcParams::default();
        let ss = SlopeSlideParams::default();
        Self {
            dist_to_run_fov: p.run_fov - p.prefab_camera_fov,
            dist_to_slide_fov: p.slide_fov - p.prefab_camera_fov,
            dist_to_run_slide_fov: p.run_slide_fov - p.prefab_camera_fov,
            run_fov: p.run_fov,
            slide_fov: p.slide_fov,
            run_slide_fov: p.run_slide_fov,
            fov: p.prefab_camera_fov,
            fov_tweens: Vec::new(),
            cc_height: p.stand_height,
            cc_center: Vec3::new(0.0, 1.0, 0.0),
            cc_skin: 0.1,
            ss_current_speed: ss.concrete_speed,
            ss_current_acceleration: 7.0,
            ss_current_deceleration: 3.0,
            p,
            ss,
            can_move: true,
            is_sprinting: false,
            is_walking: false,
            is_crouching: false,
            is_grounded: true,
            safe_grounded: false,
            is_sliding: false,
            is_leaning: false,
            is_touching_anything: false,
            touched_last_move: false,
            has_left_hand: false,
            movement_factor: 1.0,
            jump_factor: 1.0,
            wall_jump_factor: 1.0,
            max_wall_jumps: 1,
            gravity_multiplier: 1.0,
            move_direction: Vec3::ZERO,
            move_added: Vec3::ZERO,
            object_collision_move_direction: Vec3::ZERO,
            ground_normal: Vec3::ZERO,
            player_speed: 0.0,
            speed_factor: 2.92, // prefab speedFactor (recomputed each frame)
            current_input: Vec2::ZERO,
            current_input_raw: Vec2::ZERO,
            vertical_input: 0.0,
            horizontal_input: 0.0,
            look_speed_x: 2.0,
            look_speed_y: 2.0,
            look_speed_aim: 0.8,
            look_speed_aim_no_scope: 2.0,
            is_scope_aiming: false,
            rotation_x: 0.0,
            rotation_z: 0.0,
            mouse_input: Vec2::ZERO,
            gamepad: false,
            prejump: false,
            walk_slide: false,
            sprint_slide: false,
            is_zooming: false,
            is_aiming: false,
            default_fov: 75.0,
            hit_point_normal: Vec3::ZERO,
            wall_point_normal: Vec3::ZERO,
            is_sliding_steep: false,
            can_wall_jump: false,
            currentwalltag: String::new(),
            wall_jumps_count: 0,
            hit_diagonal_ceiling: false,
            wall_slide_lean: 0.0,
            land_bool: false,
            land_timer: 0.0,
            slide_end: false,
            jump_slope: false,
            on_ladder: false,
            aftervaultjump_timer: 0.0,
            slide_timer: 0.0,
            set_speed_timer: 0.0,
            slide_reset_timer: 0.0,
            slide_cancel_timer: 0.0,
            slide_time: 0.0,
            slide_dust: Vec::new(),
            coyote_timer: 0.0,
            fall_distance: 0.0,
            crouch_press: false,
            jumped: false,
            all_jumped: false,
            ground_sphere_cast: false,
            taunt_timer: 0.0,
            slope_slide_jumped: false,
            shortfront_ray: false,
            vaul_ray_down: false,
            vaul_ray_up: false,
            vault_ray_up_short: false,
            steep_slide_forward: false,
            steep_slide_backward: false,
            down_ray: true,
            anim_down_ray: false,
            slide_down_ray: false,
            up_sphere: false,
            ladder_ray: false,
            ladder_tag: String::new(),
            flymode: false,
            sprint_toggle: false,
            aim_toggle: false,
            lean_toggle: false,
            crouch_toggle: false,
            reverse_sprint_bind: false,
            invert_x: false,
            invert_y: false,
            func_sprint: false,
            func_sprint_trigger: false,
            is_leaning_left: false,
            is_leaning_right: false,
            func_lean_right_trigger: false,
            func_lean_left_trigger: false,
            temp_set_speed: 0.0,
            dir_forward: Vec3::NEG_Z,
            dir_right: Vec3::X,
            crouch_exit: false,
            prejump_land: false,
            anim_jump: false,
            activate_fall_audio: false,
            is_slide_sprinting: false,
            ground_tag: String::new(),
            jump_tag: String::new(),
            right_head_cast: false,
            left_head_cast: false,
            adding_force: false,
            force_added: Vec3::ZERO,
            force: Vec3::ZERO,
            force_factor: 0.0,
            initforce_factor: 40.0,
            tempforce_speed: 30.0,
            deceleration: 1.0,
            custom_force_final: Vec3::ZERO,
            bforcefinal: Vec3::ZERO,
            bdirection: Vec3::ZERO,
            bfactor: 0.0,
            bdecel: 0.0,
            bstop: false,
            bground_stop: false,
            baircontrol: true,
            btimer: 0.0,
            vault: false,
            vault_activate: false,
            deactivate_vault_timer: -1.0,
            rotate_back_after_vault_timer: -1.0,
            footstep_timer: 0.0,
            ladder_step_timer: 0.0,
            cc_grounded: false,
            cc_ground_contact: None,
            cc_steep_contacts: Vec::new(),
            cc_velocity: Vec3::ZERO,
            fall_audio_volume: 0.0,
            fall_audio_playing: false,
            slide_audio_volume: 0.0,
            slide_audio_playing: false,
            slide_clip: 0,
            lens_distortion: 0.0,
            saturation: 0.0,
            high_fall_vfx_alpha: 0.0,
            show_body_parts: false,
            body_parts_pending: false,
            slope_uphill: false,
            impact: Vec3::ZERO,
            slope_slide_move: Vec3::ZERO,
            steep_slope_slide_move: Vec3::ZERO,
            is_crouch_slope_sliding: false,
            slope_slide_trigger: false,
            slope_slide_trigger2: false,
            on_ice: false,
            on_super_ice: false,
            on_sand: false,
            on_concrete: false,
            sprint_slide_trigger: false,
            ss_local_slope_slide_trigger: false,
            ss_local_slope_slide_stop_trigger: false,
            ss_slide_cancel_timer: 0.0,
            ss_init_sound_volume: 0.0,
            ss_crouch_out_trigger: false,
            ss_sprint_in_trigger: false,
            ss_sprint_late_trigger: false,
            ss_stopped: false,
            ss_audio_volume: 0.0,
            ss_audio_playing: false,
            cam_rotate_back: true,
            fparms_anim: None,
            cam_base_speed: 3.0,
            last_motion: Vec3::ZERO,
            last_dt: 0.0,
            last_input: (Vec2::ZERO, 0.0, 0.0, 0.0),
            safe_death_falling: false,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Public force API (used by weapons: repulsor knockback, melee dash, explosions...)
// ---------------------------------------------------------------------------------------------

impl Fpc {
    pub fn add_force(&mut self, tempforce: Vec3, tempforce_factor: f32) {
        self.deceleration = 1.0;
        self.move_direction = Vec3::ZERO;
        self.force_factor = tempforce_factor;
        self.force = tempforce;
        self.move_direction.y = self.force.y * self.force_factor;
        self.tempforce_speed = self.p.force_speed;
        self.initforce_factor = tempforce_factor;
    }

    #[allow(dead_code)] // FirstPersonController API: called by map triggers that are not in the test map
    pub fn add_accumulated_force(&mut self, tempforce: Vec3, tempforce_factor: f32) {
        self.deceleration = 1.0;
        self.force_factor = tempforce_factor;
        self.force = tempforce;
        self.move_direction.y = self.force.y * self.force_factor;
        self.tempforce_speed = self.p.force_speed;
        self.initforce_factor = tempforce_factor;
    }

    pub fn add_horizontal_force(&mut self, tempforce: Vec3, tempforce_factor: f32) {
        self.deceleration = 1.0;
        self.move_direction = Vec3::new(0.0, self.move_direction.y, 0.0);
        self.force_factor = tempforce_factor;
        self.force = tempforce;
        self.tempforce_speed = self.p.force_speed;
        self.initforce_factor = tempforce_factor;
    }

    pub fn add_vertical_force(&mut self, tempforce: Vec3, tempforce_factor: f32) {
        self.deceleration = 1.0;
        self.move_direction = Vec3::new(self.move_direction.x, -1.0, self.move_direction.z);
        self.force_factor = tempforce_factor;
        self.force = tempforce;
        self.move_direction.y = self.force.y * self.force_factor;
        self.tempforce_speed = self.p.force_speed;
        self.initforce_factor = tempforce_factor;
    }

    /// `CustomAddForce.AddForce` (mass 3).
    pub fn custom_add_force(&mut self, dir: Vec3, force: f32) {
        const MASS: f32 = 3.0;
        self.impact += dir.normalize_or_zero() * force / MASS;
    }

    pub fn b_force(
        &mut self,
        dir: Vec3,
        factor: f32,
        vertical: bool,
        stop_on_ground: bool,
        decel: f32,
        aircontrol: bool,
    ) {
        self.bground_stop = false;
        self.bstop = false;
        self.btimer = 0.0;
        self.bdirection = dir;
        self.bfactor = factor;
        self.bdecel = decel;
        self.bground_stop = stop_on_ground;
        self.baircontrol = aircontrol;
        if vertical {
            self.move_direction.y = dir.y * factor;
        }
    }

    /// `Teleport(position, angle, boost: true, cac, power, decel, dontTranslateRotation)` (Teleporter trigger):
    /// move, turn by `-(angle - 180)` degrees of Unity yaw, BForce along the exit point's forward, teleportClip (23).
    #[allow(clippy::too_many_arguments, dead_code)] // called by the Teleporter trigger (not in the test map)
    pub fn teleport(&mut self, tf: &mut Transform, position: Vec3, angle: f32, exit_forward: Vec3, power: f32, decel: f32, dont_translate_rotation: bool, sounds: &mut MessageWriter<FpcSound>) {
        tf.translation = position;
        if !dont_translate_rotation {
            // eulerAngles -= (0, angle - 180, 0): a Unity yaw of +a is a Bevy turn of -a about Y.
            tf.rotation = Quat::from_rotation_y((angle - 180.0).to_radians()) * tf.rotation;
        }
        self.b_force(exit_forward, power, true, false, decel, true);
        if !dont_translate_rotation {
            self.dir_forward = tf.forward().as_vec3();
            self.dir_right = tf.right().as_vec3();
        }
        sounds.write(FpcSound::Indexed { index: 23, ground_tag: String::new() });
    }

    pub fn set_speed(&mut self, speed: f32, time: f32) {
        self.temp_set_speed = speed;
        self.set_speed_timer = time;
    }

    /// `KillShockWave` (camera FX on kill).
    pub fn kill_shock_wave(&mut self) {
        self.lens_distortion = self.p.kill_shock_wave_strength;
        self.saturation = -100.0;
    }

    fn get_current_offset(&self) -> f32 {
        if !self.is_crouching {
            if !self.is_sprinting {
                self.p.base_step_speed
            } else {
                self.p.base_step_speed * self.p.sprint_step_multiplier
            }
        } else {
            self.p.base_step_speed * self.p.crouch_step_multiplier
        }
    }

    fn max_speed(&self) -> f32 {
        if !self.cc_grounded && self.is_sprinting {
            self.p.sprint_air_speed
        } else if !self.cc_grounded {
            self.p.air_speed
        } else if self.is_crouching {
            self.p.crouch_speed
        } else if self.is_sprinting {
            self.p.sprint_speed
        } else {
            self.p.walk_speed
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------------------------

/// Unity `CharacterController` capsule as a Rapier shape, offset by `center`.
pub fn cc_shape(height: f32, radius: f32, center: Vec3) -> (Collider, Vec3, Quat) {
    let half = (height * 0.5 - radius).max(0.0);
    (Collider::capsule_y(half, radius), center, Quat::IDENTITY)
}

pub fn kcc(fpc: &Fpc) -> KinematicCharacterController {
    KinematicCharacterController {
        custom_shape: Some(cc_shape(fpc.cc_height, fpc.p.cc_radius, fpc.cc_center)),
        offset: CharacterLength::Absolute(fpc.cc_skin),
        up: Vec3::Y,
        slide: true,
        autostep: Some(CharacterAutostep {
            max_height: CharacterLength::Absolute(fpc.p.cc_step_offset),
            min_width: CharacterLength::Absolute(0.01),
            include_dynamic_bodies: false,
        }),
        max_slope_climb_angle: fpc.p.cc_slope_limit.to_radians(),
        // Rapier's own sliding also fires on step lips; PhysX-style force sliding off steep contacts is in the Move clamp.
        // PhysX force-sliding: slopes steeper than slopeLimit slide (90 = never, which left the player pinned on leaning
        // walls while pushing into them). Debug override RESTRAFE_MIN_SLIDE=<deg>.
        min_slope_slide_angle: std::env::var("RESTRAFE_MIN_SLIDE").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(fpc.p.cc_slope_limit).to_radians(),
        apply_impulse_to_dynamic_bodies: false,
        // Unity's CharacterController steps down onto ground within reach of its down pass; see the Move clamp.
        snap_to_ground: Some(CharacterLength::Absolute(0.1)),
        filter_groups: Some(CollisionGroups::new(
            Group::from_bits_retain(1 << physics::layer::PLAYER),
            // DynamicsManager layer matrix (see CC_COLLIDE_MASK).
            Group::from_bits_retain(CC_COLLIDE_MASK),
        )),
        filter_flags: QueryFilterFlags::EXCLUDE_SENSORS,
        ..default()
    }
}

/// Layers the local player's CharacterController collides with: the DynamicsManager m_LayerCollisionMatrix row of its
/// layer (Self) - not Interactable (weapons lying around / floating over spawners), HeldWeapon, Body, Members, HeadSup,
/// DontCollideWithPlayer, Grenade, Melee, Ragdoll. Port additions: IgnoreRaycast and its own SelfBody hit colliders
/// (triggers in the original). Also the mask of the controller's own ground probes.
const CC_COLLIDE_MASK: u32 = !((1 << physics::layer::INTERACTABLE)
    | (1 << physics::layer::HELD_WEAPON)
    | (1 << physics::layer::BODY)
    | (1 << physics::layer::MEMBERS)
    | (1 << physics::layer::HEAD_SUP)
    | (1 << physics::layer::DONT_COLLIDE_WITH_PLAYER)
    | (1 << physics::layer::GRENADE)
    | (1 << physics::layer::MELEE)
    | (1 << physics::layer::RAGDOLL)
    | (1 << physics::layer::IGNORE_RAYCAST)
    | (1 << physics::layer::SELF_BODY));

pub fn spawn_player(commands: &mut Commands, position: Vec3, yaw_unity: f32) -> Entity {
    let fpc = Fpc::default();
    let controller = kcc(&fpc);
    commands
        .spawn((
            Name::new("PlayerIK"),
            Transform::from_translation(position).with_rotation(unity_euler(0.0, yaw_unity, 0.0)),
            Visibility::default(),
            controller,
            fpc,
            // Rapier only drives character controllers attached to a collider/body. This one is a
            // sensor in no collision group, so queries never see the player (the capsule used for
            // movement is the controller's custom_shape).
            Collider::ball(0.01),
            Sensor,
            CollisionGroups::new(Group::NONE, Group::NONE),
        ))
        .with_children(|p| {
            p.spawn((
                Name::new("CameraLeanPivot"),
                LeanCamera,
                Transform::from_xyz(0.0, -0.6, 0.0),
                Visibility::default(),
            ))
            .with_children(|p| {
                p.spawn((
                    Name::new("LandBobPivot"),
                    LandBobPivot,
                    Tweens::default(),
                    Transform::from_xyz(0.0, 0.1, 0.0),
                    Visibility::default(),
                ))
                .with_children(|p| {
                    p.spawn((
                        Name::new("CameraHolder"),
                        PlayerCameraHolder,
                        Transform::from_xyz(0.0, CAMERA_HOLDER_Y, 0.0),
                        Visibility::default(),
                    ))
                    .with_children(|p| {
                        p.spawn((
                            Name::new("Camera"),
                            PlayerCamera,
                            Tweens::default(),
                            Camera3d::default(),
                            bevy::core_pipeline::prepass::DepthPrepass,
                            Projection::Perspective(PerspectiveProjection {
                                fov: 68f32.to_radians(),
                                near: 0.01,
                                far: 4000.0,
                                ..default()
                            }),
                            Transform::default(),
                            SpatialListener::new(0.2),
                        ));
                    });
                });
            });
        })
        .id()
}

// ---------------------------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------------------------

type RigQueries<'w, 's> = (
    Query<
        'w,
        's,
        &'static mut Transform,
        (
            With<LeanCamera>,
            Without<Fpc>,
            Without<LandBobPivot>,
            Without<PlayerCameraHolder>,
            Without<PlayerCamera>,
        ),
    >,
    Query<
        'w,
        's,
        (&'static mut Transform, &'static mut Tweens),
        (
            With<LandBobPivot>,
            Without<Fpc>,
            Without<LeanCamera>,
            Without<PlayerCameraHolder>,
            Without<PlayerCamera>,
        ),
    >,
    Query<
        'w,
        's,
        &'static mut Transform,
        (
            With<PlayerCameraHolder>,
            Without<Fpc>,
            Without<LeanCamera>,
            Without<LandBobPivot>,
            Without<PlayerCamera>,
        ),
    >,
    Query<
        'w,
        's,
        (
            &'static mut Transform,
            &'static mut Tweens,
            &'static mut Projection,
        ),
        (
            With<PlayerCamera>,
            Without<Fpc>,
            Without<LeanCamera>,
            Without<LandBobPivot>,
            Without<PlayerCameraHolder>,
        ),
    >,
);

fn tag_of(e: Entity, tags: &Query<&UnityTag>) -> String {
    tags.get(e)
        .map(|t| t.0.clone())
        .unwrap_or_else(|_| "Untagged".to_string())
}

/// Unity's `ControllerColliderHit` data reconstructed from a Rapier character collision.
struct CcHit {
    normal: Vec3,
    move_direction: Vec3,
    layer: u32,
    tag: String,
}

#[allow(clippy::too_many_arguments)]
pub fn fpc_update(
    time: Res<Time>,
    input: Res<PlayerInput>,
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<Settings>,
    rapier: ReadRapierContext,
    tags: Query<&UnityTag>,
    layers: Query<&physics::UnityLayer>,
    mut players: Query<(
        &mut Fpc,
        &mut Transform,
        &mut KinematicCharacterController,
        Option<&KinematicCharacterControllerOutput>,
        Option<&crate::health::PlayerHealth>,
    )>,
    mut rig: ParamSet<RigQueries>,
    mut sounds: MessageWriter<FpcSound>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let Ok(ctx) = rapier.single() else { return };
    let Ok((mut f, mut tf, mut controller, output, ph)) = players.single_mut() else {
        return;
    };
    // Dead: the player GameObject is deactivated (PlayerHealth.DespawnObjectObservers), no Update / CharacterController.
    if ph.is_some_and(|h| h.dead) {
        controller.translation = None;
        return;
    }
    let f = &mut *f;

    // ---- previous CharacterController.Move result -------------------------------------------
    let mut hits: Vec<CcHit> = Vec::new();
    let mut hit_grounded: Option<bool> = None;
    if let Some(out) = output {
        // Unity: isGrounded == CollisionFlags.Below from the last Move, which an upward move
        // never sets. Rapier's `grounded` also counts ground within the skin offset.
        // Rapier also counts any contact whose normal points up by more than 0.001 (an 89.9 deg wall); Unity's Below
        // flag needs the contact on the capsule's lower sphere, so near-vertical walls touched from the side are not
        // ground (no standing on walls, no wall-jump count resets). Ground contact normal kept for the slope clamp.
        let below = out.collisions.iter().filter_map(|c| c.hit.details.map(|d| -(c.character_rotation * d.normal2))).filter(|n| n.y > if std::env::var_os("RESTRAFE_OLD_GROUND").is_some() { 0.1 } else { 0.001 }).max_by(|a, b| a.y.total_cmp(&b.y));
        f.cc_ground_contact = below;
        {
            let walkable = f.p.cc_slope_limit.to_radians().cos();
            f.cc_steep_contacts.clear();
            for n in out.collisions.iter().filter_map(|c| c.hit.details.map(|d| -(c.character_rotation * d.normal2))) {
                let n = n.normalize_or_zero();
                if n.y > 0.001 && n.y < walkable && !f.cc_steep_contacts.iter().any(|m| m.dot(n) > 0.99) {
                    f.cc_steep_contacts.push(n);
                }
            }
        }
        // PhysX only reports CollisionFlags.Below for walkable ground: its slope test uses the sweep's contact normal, so on
        // a face steeper than slopeLimit, or hanging over a ledge with only the capsule's rounded bottom on the rim (the
        // rim contact tilts past 65 deg once the centre is ~0.36 m beyond the edge), isGrounded is false, gravity builds
        // up and the non-walkable slide takes the player off. Rapier calls any upward contact ground (also on frames
        // without a collision), so ground is confirmed by a short sphere cast under the lower capsule sphere.
        let r = f.p.cc_radius;
        let walkable = f.p.cc_slope_limit.to_radians().cos();
        let low = tf.translation + f.cc_center - Vec3::Y * (f.cc_height * 0.5 - r);
        // The cast uses the skin-inflated sphere (radius + skinWidth), as PhysX's sweeps (contactOffset) and rapier's
        // offset do: a rim contact's angle is measured against that sphere (standing on an edge until the centre is
        // about (r + skin) * sin(slopeLimit) past it).
        // Started one skin higher (a cast ignores colliders it starts inside; rapier can leave less than the full skin
        // gap, e.g. after stepping up onto a weapon spawner disc).
        let ground_ok = physics::sphere_cast(&ctx, low + Vec3::Y * f.cc_skin, r + f.cc_skin * 0.9, Vec3::NEG_Y, 0.1 + f.cc_skin * 1.1, LAND_LAYER).is_some_and(|h| h.normal.y >= walkable)
            // Rim of a step lower than stepOffset (weapon spawner discs, curbs): the FPC's large downward moveDirection
            // (OnSlopeAir) carries PhysX onto the ground below within the same Move, so isGrounded never drops there
            // (no landing bob / sound stepping off).
            || (std::env::var_os("RESTRAFE_NO_STEP_GROUND").is_none() && physics::raycast(&ctx, low, Vec3::NEG_Y, r + f.cc_skin + f.p.cc_step_offset, LAND_LAYER).is_some_and(|h| h.normal.y >= walkable));
        // OnControllerColliderHit runs inside Move: isGrounded there still holds the previous Move's flags.
        hit_grounded = if std::env::var_os("RESTRAFE_OLD_HITGROUND").is_some() { None } else { Some(f.cc_grounded) };
        f.cc_grounded = out.grounded && f.last_motion.y <= 0.0 && ground_ok;
        // PhysX moves in separate side and down passes; its down pass stops exactly contactOffset (skinWidth) above the
        // ground. Rapier keeps the offset along the combined (diagonal) motion, so a fast slide with a downward push ends
        // closer (root 0.184 instead of 0.2 measured by the heights autotest). Restore the vertical skin gap.
        if f.cc_grounded && std::env::var_os("RESTRAFE_NO_SKIN_FIX").is_none() {
            if let Some(h) = physics::sphere_cast(&ctx, low + Vec3::Y * 0.3, r, Vec3::NEG_Y, 0.3 + f.cc_skin + 0.05, LAND_LAYER) {
                let gap = h.distance - 0.3;
                if h.normal.y >= walkable && gap > -0.01 && gap < f.cc_skin - 0.002 {
                    tf.translation.y += f.cc_skin - gap;
                }
            }
        }
        if std::env::var_os("RESTRAFE_OLD_GROUND").is_some() {
            f.cc_grounded = out.grounded && f.last_motion.y <= 0.0 && (out.collisions.is_empty() || below.is_some());
        }
        if std::env::var_os("RESTRAFE_DEBUG_CC").is_some() {
            let hs: Vec<String> = out.collisions.iter().map(|c| {
                let n = c.hit.details.map(|d| d.normal2).unwrap_or(Vec3::ZERO);
                format!("[{:?} n=({:.2},{:.2},{:.2}) toi={:.3}]", c.entity, n.x, n.y, n.z, c.hit.time_of_impact)
            }).collect();
            println!("cc: want={:?} got={:?} grounded={} hits={}", f.last_motion, out.effective_translation, out.grounded, hs.join(" "));
        }
        f.cc_velocity = out.effective_translation / f.last_dt.max(1e-6);
        // ControllerColliderHit.moveDirection is the direction of the PhysX sweep pass that hit (up (0,1,0), side =
        // horizontal part of the motion, down (0,-1,0)), and hits arrive in pass order. Rapier sweeps the whole motion at
        // once, so each contact is assigned to the pass that would have met it. RESTRAFE_OLD_HITDIR=1: whole-motion dir.
        let motion = f.last_motion;
        let side = Vec3::new(motion.x, 0.0, motion.z);
        let side_dir = side.normalize_or_zero();
        let old_dir = std::env::var_os("RESTRAFE_OLD_HITDIR").is_some();
        let mut passed: Vec<(u8, CcHit)> = Vec::new();
        for c in &out.collisions {
            let normal = c.hit.details.map(|d| -(c.character_rotation * d.normal2)).unwrap_or(Vec3::Y);
            let normal = if normal.length_squared() > 0.0 {
                normal.normalize()
            } else {
                Vec3::Y
            };
            let (pass, dir) = if old_dir {
                (1, motion.normalize_or_zero())
            } else if normal.y < -0.05 && motion.y > 0.0 {
                (0, Vec3::Y)
            } else if normal.y > 0.05 && motion.y < 0.0 && (normal.y >= walkable || side.length_squared() < 1e-10) {
                (2, Vec3::NEG_Y)
            } else if side_dir != Vec3::ZERO {
                (1, side_dir)
            } else {
                (1, motion.normalize_or_zero())
            };
            passed.push((pass, CcHit {
                normal,
                move_direction: dir,
                layer: layers.get(c.entity).map(|l| l.0).unwrap_or(0),
                tag: tag_of(c.entity, &tags),
            }));
        }
        passed.sort_by_key(|(p, _)| *p);
        hits.extend(passed.into_iter().map(|(_, h)| h));
    }
    let pos = tf.translation;
    let fwd = tf.forward().as_vec3();
    let right = tf.right().as_vec3();
    let up = Vec3::Y;
    // The tail below belongs to the previous Update (its input and dt); the rest of this frame uses `mv`.
    let (tail_mv, tail_run, tail_lean_l, tail_lean_r) = f.last_input;
    let tail_dt = f.last_dt;
    let mv = input.move_raw;

    // ---- OnControllerColliderHit -------------------------------------------------------------
    f.is_touching_anything = !hits.is_empty();
    f.touched_last_move = f.is_touching_anything; // value OnTriggerStay sees (set by the last Move, before Update resets it)
    let hg = hit_grounded.unwrap_or(f.cc_grounded);
    for hit in &hits {
        if hg && hit.move_direction.y < -0.3 {
            f.hit_point_normal = hit.normal;
            let flag = physics::raycast(&ctx, pos, -up, 0.5, LAND_LAYER);
            f.is_sliding_steep = angle(hit.normal, up) > 65.0
                && (flag.as_ref().is_none() || angle(flag.unwrap().normal, up) > 65.0);
        } else {
            f.is_sliding_steep = false;
        }
        let a = angle(hit.normal, up);
        if !hg
            && f.wall_jumps_count < f.max_wall_jumps
            && !f.on_ladder
            && (hit.layer == 0 || hit.layer == 14)
            && !f.down_ray
            && a > 88.0
            && a < 100.0
        {
            if !f.can_wall_jump {
                f.slide_clip = 1;
                if !f.slide_audio_playing {
                    f.slide_audio_playing = true;
                }
                f.slide_audio_volume = 1.0;
            }
            if f.right_head_cast {
                f.wall_slide_lean = 10.0;
            } else if f.left_head_cast {
                f.wall_slide_lean = -10.0;
            }
            f.wall_point_normal = hit.normal;
            f.currentwalltag = hit.tag.clone();
            f.can_wall_jump = true;
        } else {
            f.can_wall_jump = false;
        }
        if !hg
            && hit.move_direction.y > 0.2
            && !f.hit_diagonal_ceiling
            && f.move_direction.y > 0.0
        {
            // (the original condition `> 91 || < 170` is always true)
            f.hit_diagonal_ceiling = true;
            sounds.write(FpcSound::CeilingHit);
            if a > 130.0 {
                let factor = if f.is_sprinting { -2.0 } else { -1.0 };
                f.add_vertical_force(Vec3::NEG_Y, factor);
            }
            let jsd = f.p.jump_slope_decel;
            f.b_force(
                Vec3::new(hit.normal.x, 0.0, hit.normal.z),
                5.5,
                false,
                true,
                jsd * 2.0,
                true,
            );
            if let Ok((_, mut tw, _)) = rig.p3().single_mut() {
                tw.punch_rotation(
                    -f.p.fall_shake_force,
                    f.p.fall_shake_duration,
                    f.p.fall_shake_vibrato,
                    f.p.fall_shake_elasticity,
                );
            }
        } else if f.move_direction.y > 0.0 && f.all_jumped && !(a < 170.0) && !(a > 182.0) {
            f.all_jumped = false;
            let v = if f.is_sprinting { -8.0 } else { -5.0 };
            f.add_vertical_force(Vec3::NEG_Y, v);
            sounds.write(FpcSound::CeilingHit);
            if let Ok((_, mut tw, _)) = rig.p3().single_mut() {
                let fs = f.p.fall_shake_force;
                tw.punch_rotation(
                    Vec3::new(-fs.x, 0.0, -fs.z),
                    f.p.fall_shake_duration,
                    f.p.fall_shake_vibrato,
                    f.p.fall_shake_elasticity,
                );
            }
            f.move_direction.y = v;
        }
    }

    // ---- Update() tail after the previous Move() (runs after Move in Unity) ----------------
    if f.can_move {
        if !f.cc_grounded && f.move_direction.y < 0.0 {
            f.fall_distance -= f.move_direction.y * tail_dt;
        }
        if f.move_direction.y > 0.0 {
            f.fall_distance = 0.0;
        }
        if f.cc_grounded {
            f.move_direction.y = -1.0;
            f.coyote_timer = 0.15;
            f.hit_diagonal_ceiling = false;
        }
        f.safe_grounded = (physics::ray(&ctx, pos, -up, 0.5, physics::DEFAULT_RAYCAST_LAYERS)
            && f.land_timer > 0.0
            && !f.all_jumped
            && f.move_direction.y < 0.0)
            || f.cc_grounded;
        // isAiming: zoom only when nothing is held in the left hand (PlayerPickup sets has_left_hand)
        f.is_aiming = if !f.has_left_hand { f.is_zooming } else { false };
        if f.sprint_toggle {
            if tail_run > 0.1 && f.func_sprint_trigger {
                f.func_sprint = !f.func_sprint;
                f.func_sprint_trigger = false;
            }
            if tail_run < 0.1 {
                f.func_sprint_trigger = true;
            }
        } else {
            f.func_sprint = if f.reverse_sprint_bind {
                tail_run < 0.1
            } else {
                tail_run > 0.1
            };
        }
        f.is_sprinting =
            !f.is_leaning && !f.is_aiming && tail_mv != Vec2::ZERO && !f.is_crouching && f.func_sprint;
        f.is_slide_sprinting = !f.is_leaning && !f.is_aiming && tail_mv != Vec2::ZERO && f.func_sprint;
        f.is_walking = tail_mv != Vec2::ZERO;
        f.is_grounded = f.cc_grounded;
        if f.lean_toggle {
            if tail_lean_l > 0.1 && f.func_lean_left_trigger {
                f.is_leaning_left = !f.is_leaning_left;
                f.is_leaning_right = false;
                f.func_lean_left_trigger = false;
            } else if tail_lean_r > 0.1 && f.func_lean_right_trigger {
                f.is_leaning_left = false;
                f.is_leaning_right = !f.is_leaning_right;
                f.func_lean_right_trigger = false;
            }
            if tail_lean_l < 0.1 {
                f.func_lean_left_trigger = true;
            }
            if tail_lean_r < 0.1 {
                f.func_lean_right_trigger = true;
            }
        } else {
            f.is_leaning_left = tail_lean_l > 0.1;
            f.is_leaning_right = tail_lean_r > 0.1;
            if f.is_leaning_left && f.is_leaning_right {
                f.is_leaning_left = false;
                f.is_leaning_right = false;
            }
        }
        f.is_leaning = f.is_leaning_left || f.is_leaning_right;
        if f.is_grounded {
            f.wall_jumps_count = 0;
            f.jumped = false;
            f.all_jumped = false;
            f.slope_slide_jumped = false;
        }
    }

    // ---- Input System callbacks (InputSystem dynamic update: before every MonoBehaviour.Update, so before Slope / SlopeSlide / CustomAddForce) -----------------------------------------
    if input.mouse != Vec2::ZERO || input.enabled {
        f.mouse_input = input.mouse;
    }
    f.gamepad = input.gamepad;
    // zoom: started/canceled -> SetZoom, performed -> SetZoomToggle
    if !f.aim_toggle {
        if input.zoom.pressed {
            f.is_zooming = true;
        }
        if input.zoom.released {
            f.is_zooming = false;
        }
    } else if input.zoom.performed {
        f.is_zooming = !f.is_zooming;
    }
    // crouch: started -> SetCrouch(true), performed -> Slide, canceled -> SetCrouch(false) + SlideEnd
    if input.crouch.pressed {
        if !f.crouch_toggle {
            f.is_crouching = true;
            f.crouch_press = true;
        }
        slide_pressed(f, mv);
    }
    if input.crouch.released {
        if !f.crouch_toggle {
            f.is_crouching = false;
            f.crouch_press = false;
        }
        slide_end(f);
    }
    if input.jump.pressed {
        jump(f, &ctx, pos, mv, &mut rig, &mut sounds, dt);
    }
    // ---- Slope.Update ------------------------------------------------------------------------
    {
        let front = pos + fwd * 0.121;
        let rear = pos - fwd * 0.155;
        let fd = physics::raycast(&ctx, front, -up, f32::INFINITY, 16385)
            .map(|h| h.distance)
            .unwrap_or(0.0);
        let rd = physics::raycast(&ctx, rear, -up, f32::INFINITY, 16385)
            .map(|h| h.distance)
            .unwrap_or(0.0);
        f.slope_uphill = fd < rd;
    }

    // ---- SlopeSlide.Update -------------------------------------------------------------------
    slope_slide_update(f, &ctx, pos, dt, &tags, &mut sounds);

    // ---- CustomAddForce.Update ---------------------------------------------------------------
    {
        f.custom_force_final = if f.impact.length() > 0.2 {
            f.impact
        } else {
            Vec3::ZERO
        };
        if f.is_grounded {
            f.impact.y = 0.0;
        }
        let d = if !f.is_grounded { 0.5 } else { 5.0 };
        f.impact = lerp3(f.impact, Vec3::ZERO, d * dt);
    }

    f.last_dt = dt;
    f.last_input = (mv, input.run.value(), input.lean_left.value(), input.lean_right.value());

    // ---- Settings.Update -> localPlayer ------------------------------------------------------
    let (lx, ly, la, lan) = settings.look_speeds();
    f.look_speed_x = lx;
    f.look_speed_y = ly;
    if let Some(v) = la {
        f.look_speed_aim = v;
    }
    if let Some(v) = lan {
        f.look_speed_aim_no_scope = v;
    }
    f.sprint_toggle = settings.sprint_toggle;
    f.aim_toggle = settings.aim_toggle;
    f.lean_toggle = settings.lean_toggle;
    f.invert_x = settings.invert_mouse_x;
    f.invert_y = settings.invert_mouse_y;
    f.crouch_toggle = settings.crouch_toggle;
    f.reverse_sprint_bind = settings.reverse_sprint_bind;

    // ================================ Update() ================================================
    f.is_touching_anything = false;
    // HandleTimers
    f.aftervaultjump_timer -= dt;
    f.slide_timer -= dt;
    f.slide_reset_timer -= dt;
    f.slide_cancel_timer -= dt;
    f.coyote_timer -= dt;
    f.land_timer -= dt;
    // coroutines
    if f.body_parts_pending {
        f.body_parts_pending = false;
        f.show_body_parts = true;
    }
    if f.deactivate_vault_timer >= 0.0 {
        f.deactivate_vault_timer -= dt;
        if f.deactivate_vault_timer < 0.0 {
            f.cam_rotate_back = true;
            if f.down_ray && !f.jumped {
                f.move_direction.y = -5.0;
            } else if !f.jumped {
                f.move_direction.y = 0.0;
            }
            f.rotate_back_after_vault_timer = 0.3;
        }
    }
    if f.rotate_back_after_vault_timer >= 0.0 {
        f.rotate_back_after_vault_timer -= dt;
        if f.rotate_back_after_vault_timer < 0.0 {
            f.cam_rotate_back = true;
            f.vault = false;
        }
    }

    f.default_fov = settings.normal_fov_value;
    f.run_fov = f.default_fov + f.dist_to_run_fov;
    f.slide_fov = f.default_fov + f.dist_to_slide_fov;
    f.run_slide_fov = f.default_fov + f.dist_to_run_slide_fov;

    if tf.translation.y < -300.0 && !f.safe_death_falling {
        f.safe_death_falling = true; // respawn handled by PlayerHealth (fellVoid)
    }
    if keys.just_pressed(KeyCode::KeyL) && keys.pressed(KeyCode::ControlLeft) {
        f.flymode = !f.flymode; // test map only in the original
    }
    let skin = if f.is_crouching && !f.is_sliding {
        0.07
    } else {
        0.2
    };
    // Unity CharacterController overlap recovery pushes out immediately when the skin grows;
    // Rapier would creep out over seconds.
    if skin > f.cc_skin && f.cc_grounded {
        tf.translation.y += skin - f.cc_skin;
    }
    f.cc_skin = skin;
    f.saturation = lerp(f.saturation, 0.0, f.p.saturation_speed * dt);
    f.dir_forward = lerp3(f.dir_forward, fwd, 13.0 * dt);
    f.dir_right = lerp3(f.dir_right, right, 13.0 * dt);

    if input.enabled {
        handle_mouse_look(f, &mut tf, &mut rig);
        handle_camera_controller(f, mv, dt);
        if f.can_move {
            calculate_movement_input(f, mv, dt);
            handle_physics_casting(f, &ctx, &tf, &tags);
            check_for_vault(f, &ctx, &tf, mv, dt, &mut rig, &mut sounds, &tags);
            handle_ladders(f, &ctx, &tf, &input, dt, &mut sounds);
            if !f.flymode {
                handle_camera_lean(f, &ctx, &tf, &mut rig, dt);
            }
            handle_crouch(f, &ctx, &tf, mv, dt, &mut sounds);
            // HandleHeadMovement
            if let Ok(mut lean) = rig.p0().single_mut() {
                lean.translation = f.cc_center + Vec3::new(0.0, f.cc_height / 2.0 - 2.5, 0.0);
            }
            handle_footsteps(f, &ctx, &tf, dt, &tags, &mut sounds);
        }
    } else {
        f.current_input = Vec2::ZERO;
    }
    handle_bforce(f, dt);
    if !f.flymode {
        // HandleMovementInput
        if f.can_move {
            let ms = f.max_speed();
            f.current_input = clamp_mag2(f.current_input, ms) * f.movement_factor;
        } else {
            f.current_input = Vec2::ZERO;
        }
        let y = f.move_direction.y;
        f.move_direction = f.dir_forward * f.current_input.x + f.dir_right * f.current_input.y;
        f.move_direction.y = y;
    }
    handle_adding_force(f, dt);
    if input.enabled {
        handle_taunt(f, &input, dt, &mut sounds);
    }

    let mut motion = Vec3::ZERO;
    if !f.flymode {
        motion = apply_final_movements(f, &ctx, &tf, mv, dt, &mut rig, &mut sounds);
    }
    if f.can_move {
        if f.flymode {
            let n = (keys.pressed(KeyCode::Space) || keys.pressed(KeyCode::KeyE)) as i32
                - (keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::KeyQ)) as i32;
            let m = mv.y * fwd + mv.x * right + Vec3::Y * n as f32;
            motion += m * dt * 10.0 * if f.is_sprinting { 2.0 } else { 1.0 };
        }
    }

    // ---- CharacterController.Move -----------------------------------------------------------
    // Unity's CharacterController moves in an up / side / down pass and the down pass stops on ground within the
    // slope limit instead of sliding along it. Rapier slides the whole translation, which turned the huge
    // OnSlopeAir push (gravity * slopeForce) into downhill speed and cancelled walking uphill. On walkable ground the
    // downward part therefore only follows the ground plane under the horizontal move plus a small stick distance.
    let mut motion = motion;
    if motion.y < 0.0 && !f.flymode {
        // Down pass: ground straight under the capsule bottom (the rapier offset keeps the capsule cc_skin above it).
        let bottom = tf.translation + f.cc_center - Vec3::Y * (f.cc_height * 0.5);
        let reach = -motion.y + f.cc_skin + 0.3;
        let ground = physics::raycast(&ctx, bottom + Vec3::Y * 0.3, Vec3::NEG_Y, reach + 0.3, CC_COLLIDE_MASK);
        let walkable = f.p.cc_slope_limit.to_radians().cos();
        // PhysX CCT non-walkable mode (Unity: prevent climbing and force sliding): resting only on a contact steeper than
        // slopeLimit (a wall edge) with no walkable ground straight below, the downward move slides along that contact.
        // Several steep contacts (a V crease between two faces, e.g. running into a corner of leaning walls): the
        // projection on one face points into the other and rapier cancels it, so the slide follows the crease line.
        let steep = if f.cc_steep_contacts.is_empty() { f.cc_ground_contact.filter(|c| c.y < walkable).into_iter().collect() } else { f.cc_steep_contacts.clone() };
        if !steep.is_empty() && ground.as_ref().is_none_or(|h| h.normal.y < walkable || h.distance > 0.3 + f.cc_skin + 0.1) {
            let down = Vec3::new(0.0, motion.y, 0.0);
            let mut slide = down;
            for n in &steep {
                let d = slide.dot(*n);
                if d < 0.0 {
                    slide -= *n * d;
                }
            }
            if steep.iter().any(|n| slide.dot(*n) < -1e-5) {
                let crease = steep.iter().enumerate().flat_map(|(i, a)| steep[i + 1..].iter().map(move |b| a.cross(*b))).find(|c| c.length() > 0.05);
                if let Some(c) = crease {
                    let mut dir = c.normalize();
                    if dir.y > 0.0 {
                        dir = -dir;
                    }
                    slide = dir * down.dot(dir);
                }
            }
            // Horizontal input into a non-walkable face (PhysX prevents climbing it): rapier would turn that push into a
            // climb, cancel it and with it the frame's downward slide (min_slope_slide 90), leaving the player stuck on
            // the face. Keep only its component along the faces. Near-vertical walls (>= 88 deg) keep their hits for
            // the wall-jump check.
            let mut h = Vec3::new(motion.x, 0.0, motion.z);
            for n in steep.iter().filter(|n| n.y >= 0.035) {
                let nh = Vec3::new(n.x, 0.0, n.z).normalize_or_zero();
                let d = h.dot(nh);
                if d < 0.0 {
                    h -= nh * d;
                }
            }
            motion = Vec3::new(h.x + slide.x, slide.y, h.z + slide.z);
        }
        if let Some(h) = ground {
            let n = f.cc_ground_contact.filter(|c| c.y >= h.normal.y).unwrap_or(h.normal);
            if n.y > walkable {
                // Vertical clearance of the (radius + skin) sphere above a plane of normal n: (r + skin) / n.y - r.
                let r = f.p.cc_radius;
                let gap = (h.distance - 0.3 - ((r + f.cc_skin) / h.normal.y.max(0.1) - r)).max(0.0);
                let follow = -(n.x * motion.x + n.z * motion.z) / n.y;
                // Exactly along the ground plane (after closing any gap): an extra push into walkable ground makes rapier
                // stop at the contact and drop the frame's horizontal move, or slide it sideways into huge speeds.
                motion.y = motion.y.max(follow.min(0.0) - gap);
            }
        }
    }
    f.last_motion = motion;
    controller.translation = Some(motion);
    controller.custom_shape = Some(cc_shape(f.cc_height, f.p.cc_radius, f.cc_center));
    controller.offset = CharacterLength::Absolute(f.cc_skin);
}

fn slope_slide_update(
    f: &mut Fpc,
    ctx: &RapierContext,
    pos: Vec3,
    dt: f32,
    tags: &Query<&UnityTag>,
    sounds: &mut MessageWriter<FpcSound>,
) {
    let _ = sounds;
    if f.is_sprinting && (f.is_crouching || f.is_sliding) {
        f.sprint_slide_trigger = true;
        f.ss_sprint_late_trigger = true;
    }
    let hit = physics::raycast(ctx, pos, Vec3::NEG_Y, 8.0, LAND_LAYER);
    let down_ray = hit.is_some();
    let normal = hit.as_ref().map(|h| h.normal).unwrap_or(Vec3::ZERO);
    let a = angle(normal, Vec3::Y);
    f.is_crouch_slope_sliding = f.safe_grounded
        && down_ray
        && f.is_crouching
        && a > f.ss.min_slope_angle
        && (f.on_ice
            || (!f.is_sprinting && f.is_sliding)
            || f.sprint_slide_trigger
            || f.slope_slide_trigger2);
    let span = f.ss.second_clamp - f.ss.first_clamp;
    let speed_factor = a.clamp(f.ss.first_clamp, f.ss.second_clamp) / span;
    let walk_speed_factor = a.clamp(f.ss.walk_first_clamp, f.ss.walk_second_clamp) / span;
    if f.is_crouch_slope_sliding {
        f.slope_slide_move = lerp3(
            f.slope_slide_move,
            Vec3::new(normal.x, -normal.y, normal.z) * speed_factor * f.ss_current_speed,
            f.ss_current_acceleration * dt,
        );
        f.ss_local_slope_slide_stop_trigger = true;
    } else {
        f.slope_slide_move = lerp3(
            Vec3::new(f.slope_slide_move.x, 0.0, f.slope_slide_move.z),
            Vec3::ZERO,
            f.ss_current_deceleration * dt,
        );
        f.sprint_slide_trigger = false;
        f.slope_slide_trigger2 = false;
        f.ss_local_slope_slide_trigger = true;
        f.slope_slide_trigger = true;
    }
    if f.on_ice && f.safe_grounded {
        let target = Vec3::new(normal.x, 0.0, normal.z)
            * lerp(
                f.ss.min_walk_ice_slide_speed,
                f.ss.max_walk_ice_slide_speed,
                walk_speed_factor,
            );
        f.steep_slope_slide_move = lerp3(
            f.steep_slope_slide_move,
            target,
            f.ss_current_acceleration * dt,
        );
    } else {
        f.steep_slope_slide_move = lerp3(
            Vec3::new(f.steep_slope_slide_move.x, 0.0, f.steep_slope_slide_move.z),
            Vec3::ZERO,
            f.ss.steep_slope_slide_deceleration * dt,
        );
    }
    if !f.is_crouching && !f.on_ice {
        f.slope_slide_move = Vec3::ZERO;
    }
    if f.ss_local_slope_slide_trigger
        && f.sprint_slide_trigger
        && f.is_crouch_slope_sliding
        && f.ss_sprint_late_trigger
    {
        f.ss_sprint_late_trigger = false;
        f.ss_sprint_in_trigger = true;
        f.ss_local_slope_slide_trigger = false;
        f.ss_slide_cancel_timer = 2.1;
        f.ss_audio_volume = 1.0;
        f.ss_init_sound_volume = f.ss_audio_volume;
        f.ss_audio_playing = true;
        f.ss_stopped = true;
    } else if f.ss_local_slope_slide_trigger
        && f.is_crouch_slope_sliding
        && f.ss_sprint_late_trigger
    {
        f.slope_slide_trigger2 = true;
        f.ss_sprint_late_trigger = false;
        f.ss_sprint_in_trigger = true;
        f.ss_local_slope_slide_trigger = false;
        f.ss_slide_cancel_timer = 2.1;
        f.ss_audio_volume = 1.0;
        f.ss_init_sound_volume = f.ss_audio_volume;
        f.ss_stopped = true;
    }
    if f.is_crouching {
        f.ss_crouch_out_trigger = true;
    }
    if f.ss_crouch_out_trigger
        && !f.is_crouching
        && f.ss_audio_volume > 0.1
        && f.ss_sprint_in_trigger
    {
        f.ss_init_sound_volume = f.ss_audio_volume;
        f.ss_sprint_in_trigger = false;
        f.ss_crouch_out_trigger = false;
        f.ss_slide_cancel_timer = 0.0;
    }
    // SlideFadeOut
    let duration = if !f.is_crouching {
        0.2
    } else if f.on_ice {
        f.ss.ice_fade_out_time
    } else if f.on_concrete {
        f.ss.concrete_fade_out_time
    } else {
        f.ss.sand_fade_out_time
    };
    f.ss_slide_cancel_timer += dt;
    if f.ss_slide_cancel_timer <= duration {
        f.ss_audio_volume = lerp(
            f.ss_init_sound_volume,
            0.0,
            f.ss_slide_cancel_timer / duration,
        );
    }
    if (f.ss_audio_volume < 0.05 || (f.ss_slide_cancel_timer > 1.0 && !f.is_crouching))
        && f.ss_stopped
    {
        f.ss_stopped = false;
        f.ss_audio_playing = false;
    }
    if f.ss_local_slope_slide_stop_trigger && !f.is_crouch_slope_sliding {
        f.ss_init_sound_volume = f.ss_audio_volume;
        f.ss_local_slope_slide_stop_trigger = false;
        f.ss_slide_cancel_timer = 0.0;
    }
    if let Some(h) = hit {
        let tag = tag_of(h.entity, tags);
        let (ice, sup, sand) = match tag.as_str() {
            "Footsteps/Ice" => (true, false, false),
            "Footsteps/SuperIce" => (true, true, false),
            "Footsteps/Sand" => (false, false, true),
            _ => (false, false, false),
        };
        f.on_ice = ice;
        f.on_super_ice = sup;
        f.on_sand = sand;
        f.on_concrete = !ice && !sand;
        let s = &f.ss;
        (
            f.ss_current_speed,
            f.ss_current_acceleration,
            f.ss_current_deceleration,
        ) = if ice {
            (s.ice_speed, s.ice_acceleration, s.ice_deceleration)
        } else if sand {
            (s.sand_speed, s.sand_acceleration, s.sand_deceleration)
        } else {
            (
                s.concrete_speed,
                s.concrete_acceleration,
                s.concrete_deceleration,
            )
        };
    }
}

fn slide_pressed(f: &mut Fpc, mv: Vec2) {
    if f.crouch_toggle {
        f.is_crouching = !f.is_crouching;
        if !f.is_crouching {
            end_slide(f);
        }
    }
    if mv.y < 0.0 {
        return;
    }
    f.deceleration = 1.0;
    if mv != Vec2::ZERO && f.slide_reset_timer < 0.0 {
        if f.func_sprint {
            f.slide_timer = 0.8;
        }
        f.slide_reset_timer = f.p.slide_reset_time;
    }
}

fn slide_end(f: &mut Fpc) {
    if !f.crouch_toggle {
        end_slide(f);
    }
}

fn end_slide(f: &mut Fpc) {
    f.is_sliding = false;
    if f.force_factor > 1.0 {
        f.slide_end = true;
    }
    f.is_crouching = false;
    f.crouch_press = false;
    f.slide_timer = 0.0;
    f.sprint_slide = false;
    f.walk_slide = false;
    if f.cc_grounded {
        f.slide_cancel_timer = 2.0;
    }
    if !f.is_crouch_slope_sliding {
        f.show_body_parts = false;
    }
}

fn punch_camera(
    rig: &mut ParamSet<RigQueries>,
    punch: Vec3,
    duration: f32,
    vibrato: i32,
    elasticity: f32,
) {
    if let Ok((_, mut tw, _)) = rig.p3().single_mut() {
        tw.punch_rotation(punch, duration, vibrato, elasticity);
    }
}

fn jump(
    f: &mut Fpc,
    ctx: &RapierContext,
    pos: Vec3,
    mv: Vec2,
    rig: &mut ParamSet<RigQueries>,
    sounds: &mut MessageWriter<FpcSound>,
    dt: f32,
) {
    if !f.can_move || f.on_super_ice {
        return;
    }
    let shake = |f: &Fpc, rig: &mut ParamSet<RigQueries>, mult: f32| {
        punch_camera(
            rig,
            -f.p.fall_shake_force * mult,
            f.p.fall_shake_duration,
            f.p.fall_shake_vibrato,
            f.p.fall_shake_elasticity,
        );
    };
    let slope_force = if mv != Vec2::ZERO {
        f.p.jump_slope_force
    } else {
        7.0
    };
    if f.can_wall_jump {
        f.wall_jumps_count += 1;
        slide_fade_out(f, dt); // SlideFadeOut uses Time.deltaTime
        sounds.write(FpcSound::WallJump {
            wall_tag: f.currentwalltag.clone(),
        });
        sounds.write(FpcSound::Indexed {
            index: 22,
            ground_tag: String::new(),
        });
        let (n, d) = (f.wall_point_normal, f.p.jump_slope_decel);
        f.b_force(n, slope_force * 0.7, false, true, d, true);
        f.move_direction.y = f.p.jump_force * 0.8 * f.wall_jump_factor;
        shake(f, rig, 1.5);
        f.can_wall_jump = false;
    }
    if f.cc_grounded {
        f.jump_slope = true;
        f.move_direction.y = 0.0;
    }
    if f.is_crouch_slope_sliding {
        f.slope_slide_move.y = 0.0;
    }
    f.jump_slope = true;
    if physics::ray(ctx, pos, Vec3::NEG_Y, 1.3, physics::DEFAULT_RAYCAST_LAYERS)
        && f.move_direction.y < 0.0
        && !f.cc_grounded
    {
        f.prejump = true;
    }
    let do_jump =
        |f: &mut Fpc, rig: &mut ParamSet<RigQueries>, sounds: &mut MessageWriter<FpcSound>| {
            f.move_direction.y = f.p.jump_force * f.jump_factor;
            f.slide_cancel_timer = 2.0;
            f.anim_jump = true; // networkAnimator.SetTrigger("Jump")
            if f.is_sliding_steep {
                let (n, d) = (f.hit_point_normal, f.p.jump_slope_decel);
                f.b_force(n, slope_force, false, true, d, true);
                shake(f, rig, 1.0);
            }
            sounds.write(FpcSound::Indexed {
                index: 10,
                ground_tag: f.jump_tag.clone(),
            });
        };
    if f.cc_grounded && !f.prejump {
        f.all_jumped = true;
        f.slope_slide_jumped = true;
        f.coyote_timer = 0.0;
        do_jump(f, rig, sounds);
    } else if f.coyote_timer > 0.0 && !f.cc_grounded && f.move_direction.y < 0.0 {
        f.all_jumped = true;
        f.slope_slide_jumped = true;
        f.prejump = false;
        f.coyote_timer = 0.0;
        do_jump(f, rig, sounds);
    } else if f.aftervaultjump_timer > 0.0 {
        f.all_jumped = true;
        f.jumped = true;
        f.slope_slide_jumped = true;
        f.vault_activate = true;
        f.prejump = false;
        f.aftervaultjump_timer = 0.0;
        f.move_direction.y = 0.0;
        do_jump(f, rig, sounds);
    }
}

/// FirstPersonController.SlideFadeOut (wall-slide audio).
fn slide_fade_out(f: &mut Fpc, dt: f32) {
    if f.slide_cancel_timer > 0.0 {
        f.slide_audio_volume = lerp(f.slide_audio_volume, 0.0, 5.0 * dt);
    }
    if f.slide_audio_volume < 0.03 {
        f.slide_audio_playing = false;
    }
}

fn handle_mouse_look(f: &mut Fpc, tf: &mut Transform, rig: &mut ParamSet<RigQueries>) {
    let sens_y = if f.is_scope_aiming {
        f.look_speed_aim
    } else if f.is_aiming {
        f.look_speed_aim_no_scope
    } else {
        f.look_speed_y
    };
    let sens_x = if f.is_scope_aiming {
        f.look_speed_aim
    } else if f.is_aiming {
        f.look_speed_aim_no_scope
    } else {
        f.look_speed_x
    };
    // `* (pauseManager.gamepad ? 14 : 1)`: per frame, no deltaTime (as in the original).
    let pad = if f.gamepad { 14.0 } else { 1.0 };
    f.rotation_x -= f.mouse_input.y * 0.1 * sens_y * if f.invert_y { -1.0 } else { 1.0 } * pad;
    f.rotation_x = f
        .rotation_x
        .clamp(-f.p.upper_look_limit, f.p.lower_look_limit);
    if let Ok(mut holder) = rig.p2().single_mut() {
        holder.rotation = unity_euler(f.rotation_x, 0.0, f.rotation_z);
    }
    let yaw = f.mouse_input.x * 0.1 * if f.invert_x { -1.0 } else { 1.0 } * sens_x * pad;
    tf.rotation *= unity_euler(0.0, yaw, 0.0);
}

fn handle_camera_controller(f: &mut Fpc, mv: Vec2, dt: f32) {
    let p = &f.p;
    let mut num = if f.is_aiming {
        p.zoom_fov
    } else if (f.is_sliding && f.is_sprinting) || f.is_crouch_slope_sliding {
        f.run_slide_fov
    } else if f.is_sliding {
        f.slide_fov
    } else if f.is_walking && f.func_sprint {
        f.run_fov
    } else {
        f.default_fov
    };
    num = round2(num);
    if f.fov != num {
        // ChangeFOV: a new DOFieldOfView every frame while fieldOfView != num. All of them keep running; DOTween
        // updates them in creation order (each captures the current FOV as its start on its first update) and the
        // newest writes last, so the transition is much quicker than a single tween.
        let (ease, speed) = if f.sprint_slide {
            (p.run_slide_ease, p.run_slide_ease_speed)
        } else if f.walk_slide {
            (p.slide_ease, p.slide_ease_speed)
        } else if f.is_sprinting {
            (p.run_ease, p.run_ease_speed)
        } else {
            (p.zoom_ease, p.zoom_speed)
        };
        f.fov_tweens.push((None, 0.0, num, ease, speed));
    }
    // DOTween update (runs after the FPC's Update).
    let mut fov = f.fov;
    for (start, elapsed, to, ease, dur) in f.fov_tweens.iter_mut() {
        let s = *start.get_or_insert(fov);
        *elapsed = (*elapsed + dt).min(*dur);
        let k = if *dur > 0.0 { ease.eval(*elapsed / *dur) } else { 1.0 };
        fov = s + (*to - s) * k;
    }
    f.fov = fov;
    if std::env::var_os("RESTRAFE_DEBUG_FOV").is_some() {
        println!("FOVDBG fov={fov:.2} n={}", f.fov_tweens.len());
    }
    f.fov_tweens.retain(|t| t.1 < t.4);
    let temptilt = (if f.is_sliding || f.is_crouch_slope_sliding {
        -p.slide_tilt_amount
    } else if f.is_sprinting && mv.x > 0.0 {
        -p.tilt_amount
    } else if f.is_sprinting && mv.x < 0.0 {
        p.tilt_amount
    } else {
        0.0
    }) + f.wall_slide_lean;
    // SideTilt (root localRotation.z is 0 for a yaw-only body)
    let b = (-f.mouse_input.x * 0.1 * p.rotation_amount)
        .clamp(-p.max_rotation_amount, p.max_rotation_amount);
    f.rotation_z = lerp(f.rotation_z, temptilt, p.tilt_speed * dt)
        + lerp(0.0, b, dt * p.smooth_rotation * 2.3);
}

fn calculate_movement_input(f: &mut Fpc, mv: Vec2, dt: f32) {
    let p = f.p.clone();
    f.player_speed = round2(f.cc_velocity.length());
    let ac = if !f.cc_grounded { p.air_control } else { 1.0 };
    let axis = |cur: f32, input: f32, bair: bool| -> f32 {
        if input > 0.0 && bair {
            lerp(cur, 1.0, p.global_acceleration * ac * dt)
        } else if input < 0.0 && bair {
            lerp(cur, -1.0, p.global_acceleration * ac * dt)
        } else {
            lerp(cur, 0.0, p.global_deceleration * ac * dt)
        }
    };
    f.horizontal_input = axis(f.horizontal_input, mv.x, f.baircontrol);
    f.vertical_input = axis(f.vertical_input, mv.y, f.baircontrol);
    let moving = mv.length() != 0.0;
    let to = |sf: f32, target: f32, acc: f32| round2(lerp(sf, target, acc * dt));
    if !f.is_crouch_slope_sliding || !f.slope_uphill {
        f.speed_factor = if !f.cc_grounded && f.func_sprint && moving && !f.is_leaning {
            to(
                f.speed_factor,
                p.sprint_air_speed,
                p.sprint_air_acceleration,
            )
        } else if !f.cc_grounded && moving && !f.is_leaning {
            to(f.speed_factor, p.air_speed, p.air_acceleration)
        } else if f.is_crouching && moving && f.cc_grounded && !f.is_leaning {
            to(f.speed_factor, p.crouch_speed, p.crouch_acceleration)
        } else if f.is_sprinting && moving && !f.is_leaning {
            to(f.speed_factor, p.sprint_speed, p.sprint_acceleration)
        } else if moving && !f.is_leaning {
            to(f.speed_factor, p.walk_speed, p.walk_acceleration)
        } else {
            to(f.speed_factor, 1.0, p.global_deceleration)
        };
    } else {
        f.speed_factor = to(f.speed_factor, 1.0, p.global_deceleration);
    }
    f.set_speed_timer -= dt;
    if f.set_speed_timer > 0.0 {
        f.speed_factor = f.temp_set_speed;
    }
    f.current_input =
        Vec2::new(round2(f.vertical_input), round2(f.horizontal_input)) * f.speed_factor;
    f.current_input_raw = f.max_speed() * mv;
}

fn handle_physics_casting(
    f: &mut Fpc,
    ctx: &RapierContext,
    tf: &Transform,
    tags: &Query<&UnityTag>,
) {
    let pos = tf.translation;
    let fwd = tf.forward().as_vec3();
    let right = tf.right().as_vec3();
    let d = physics::DEFAULT_RAYCAST_LAYERS;
    f.shortfront_ray = physics::ray(ctx, pos - Vec3::Y * f.cc_skin, fwd, 0.6, d);
    f.right_head_cast = physics::ray(ctx, pos, right, 2.0, LAND_LAYER);
    f.left_head_cast = physics::ray(ctx, pos, -right, 2.0, LAND_LAYER);
    if !f.can_wall_jump {
        f.wall_slide_lean = 0.0;
    }
    match physics::raycast(ctx, pos, Vec3::NEG_Y, 8.0, LAND_LAYER) {
        Some(h) => {
            f.ground_normal = h.normal;
            f.ground_tag = tag_of(h.entity, tags);
        }
        None => f.ground_normal = Vec3::ZERO,
    }
    // FirstPersonController.JumpClipObservers: Physics.Raycast(transform.position, down, 3f) on the default layers,
    // switch on hitInfo.collider.tag (unknown / Untagged -> concreteClips), else jumpClip.
    f.jump_tag = match physics::raycast(ctx, pos, Vec3::NEG_Y, 3.0, d) {
        Some(h) => {
            let t = tag_of(h.entity, tags);
            if t.is_empty() { "Untagged".to_string() } else { t }
        }
        None => String::new(),
    };
    let steep = |h: Option<physics::RayHit>, min: f32| {
        h.map(|h| angle(h.normal, Vec3::Y) > min).unwrap_or(false)
    };
    f.vaul_ray_down = steep(physics::raycast(ctx, pos, fwd, 1.5, d), 88.0);
    f.ground_sphere_cast = physics::check_sphere(ctx, pos, 0.28, LAND_LAYER);
    f.steep_slide_forward =
        steep(physics::raycast(ctx, pos, fwd, 1.5, d), 65.0) && f.is_sliding_steep;
    f.steep_slide_backward =
        steep(physics::raycast(ctx, pos, -fwd, 1.5, d), 65.0) && f.is_sliding_steep;
    let ladders = physics::overlap_sphere(
        ctx,
        pos + Vec3::Y * f.p.ladder_detection_length,
        f.p.ladder_detection_length,
        LADDER_LAYER,
    );
    f.ladder_ray = !ladders.is_empty();
    f.ladder_tag = ladders
        .first()
        .map(|e| tag_of(*e, tags))
        .unwrap_or_default();
    f.vaul_ray_up = steep(physics::raycast(ctx, pos + Vec3::Y, fwd, 1.5, d), 88.0);
    f.vault_ray_up_short = physics::ray(ctx, pos + Vec3::Y / 2.0, fwd, 1.3, d);
    f.down_ray = physics::ray(ctx, pos, Vec3::NEG_Y, 0.3, d);
    f.anim_down_ray = physics::ray(ctx, pos, Vec3::NEG_Y, 0.6, d);
    f.slide_down_ray = physics::ray(ctx, pos, Vec3::NEG_Y, 0.8, d);
    f.up_sphere = physics::ray(ctx, pos + Vec3::Y * f.cc_height, Vec3::Y, 0.25, LAND_LAYER);
}

#[allow(clippy::too_many_arguments)]
fn check_for_vault(
    f: &mut Fpc,
    ctx: &RapierContext,
    tf: &Transform,
    mv: Vec2,
    dt: f32,
    rig: &mut ParamSet<RigQueries>,
    sounds: &mut MessageWriter<FpcSound>,
    tags: &Query<&UnityTag>,
) {
    f.aftervaultjump_timer -= dt;
    if f.is_grounded {
        f.vault_activate = true;
    }
    if f.safe_grounded || f.is_crouching {
        return;
    }
    let pos = tf.translation;
    let fwd = tf.forward().as_vec3();
    let hit = physics::raycast(ctx, pos, fwd, 1.4, LAND_LAYER).filter(|_| !f.is_grounded);
    let flag = physics::ray(ctx, pos + Vec3::Y * 1.2, fwd, 1.5, LAND_LAYER);
    let flag2 = physics::ray(ctx, pos + Vec3::Y * 1.8, fwd, 2.0, LAND_LAYER);
    let flag3 = physics::ray(ctx, pos, Vec3::NEG_Y, 1.0, LAND_LAYER);
    if let Some(h) = hit {
        let a = angle(h.normal, Vec3::Y);
        if !flag
            && !flag2
            && a > f.p.vault_minimum_angle
            && a < 130.0
            && f.vault_activate
            && mv.y > 0.0
            && (!(f.move_direction.y > 0.0) || !flag3)
        {
            f.jumped = false;
            f.slope_slide_jumped = false;
            f.vault_activate = false;
            f.aftervaultjump_timer = 0.5;
            punch_camera(rig, Vec3::new(6.0, 0.0, 3.0), 0.45, 0, 3.0);
            f.vault = true;
            f.move_direction.y = 9.0;
            f.b_force(fwd, 1.5, false, true, 3.0, true);
            sounds.write(FpcSound::Vault {
                surface_tag: tag_of(h.entity, tags),
            });
            f.deactivate_vault_timer = 0.15;
        }
    }
}

fn handle_ladders(
    f: &mut Fpc,
    ctx: &RapierContext,
    tf: &Transform,
    input: &PlayerInput,
    dt: f32,
    sounds: &mut MessageWriter<FpcSound>,
) {
    // HandleLadderSounds
    if f.on_ladder && f.ladder_ray {
        f.ladder_step_timer -= dt;
        if f.ladder_step_timer <= 0.0 {
            let idx = if f.ladder_tag == "Ladder/Chain" {
                18
            } else {
                17
            };
            sounds.write(FpcSound::Indexed {
                index: idx,
                ground_tag: f.ladder_tag.clone(),
            });
            f.ladder_step_timer = f.p.ladder_step;
        }
    }
    let mv = input.move_raw;
    if f.ladder_ray && !f.is_crouching {
        if mv.length() > 0.0 || input.move_up.value() > 0.1 {
            f.on_ladder = true;
            f.move_direction.y = f.p.ladder_speed;
        } else {
            f.on_ladder = false;
        }
    } else {
        f.on_ladder = false;
    }
    if f.is_crouching {
        f.on_ladder = false;
    }
    let pos = tf.translation;
    let fwd = tf.forward().as_vec3();
    if f.on_ladder && physics::ray(ctx, pos, fwd, 1.0, LADDER_LAYER) && mv.y < 0.0 {
        f.b_force(-fwd, 7.0, false, true, 3.0, true);
    }
    if f.on_ladder && physics::ray(ctx, pos, -fwd, 1.0, LADDER_LAYER) && mv.y < 0.0 {
        f.b_force(fwd, 7.0, false, true, 3.0, true);
    }
}

fn handle_camera_lean(
    f: &mut Fpc,
    ctx: &RapierContext,
    tf: &Transform,
    rig: &mut ParamSet<RigQueries>,
    dt: f32,
) {
    let target = if f.is_leaning_right && !f.is_sliding && f.is_grounded {
        unity_euler(0.0, 0.0, -f.p.lean_limit)
    } else if f.is_leaning_left && !f.is_sliding && f.is_grounded {
        unity_euler(0.0, 0.0, f.p.lean_limit)
    } else {
        Quat::IDENTITY
    };
    let head = tf.translation + Vec3::Y * 2.0;
    let right = tf.right().as_vec3();
    let left_hit = physics::raycast(ctx, head, -right, 1.8, LAND_LAYER);
    let right_hit = physics::raycast(ctx, head, right, 1.8, LAND_LAYER);
    // playerCamera world position ≈ lean pivot + bob + holder offset (rotation-free approximation)
    let mut lean_q = rig.p0();
    let Ok(mut lean) = lean_q.single_mut() else {
        return;
    };
    let cam_pos = tf.transform_point(lean.translation + Vec3::new(0.0, CAMERA_HOLDER_Y + 0.1, 0.0));
    let speed = f.p.lean_speed;
    let approach = |lean: &mut Transform, hit: &physics::RayHit| {
        let frac = head.distance(hit.point) / 1.8;
        let goal = slerp(Quat::IDENTITY, target, frac);
        let s = if cam_pos.distance(hit.point) < 0.8 {
            200.0
        } else {
            speed
        };
        lean.rotation = slerp(lean.rotation, goal, s * dt);
    };
    if f.is_leaning_right {
        match &right_hit {
            Some(h) => approach(&mut lean, h),
            None => lean.rotation = slerp(lean.rotation, target, speed * dt),
        }
    } else if f.is_leaning_left {
        match &left_hit {
            Some(h) => approach(&mut lean, h),
            None => lean.rotation = slerp(lean.rotation, target, speed * dt),
        }
    } else {
        lean.rotation = slerp(lean.rotation, Quat::IDENTITY, speed * dt);
    }
}

fn handle_crouch(
    f: &mut Fpc,
    ctx: &RapierContext,
    tf: &Transform,
    mv: Vec2,
    dt: f32,
    _sounds: &mut MessageWriter<FpcSound>,
) {
    let pos = tf.translation;
    if physics::ray(ctx, pos, Vec3::Y, 2.0, LAND_LAYER) {
        f.crouch_exit = true;
        f.is_crouching = true;
    } else if f.crouch_exit && !f.crouch_press {
        f.is_crouching = false;
        f.crouch_exit = false;
    }
    let num = if f.is_sliding || f.is_crouch_slope_sliding {
        f.p.slide_height
    } else if f.is_crouching {
        f.p.crouch_height
    } else {
        f.p.stand_height
    };
    if f.cc_height != num {
        // AdjustHeight
        let k = f.p.crouching_speed * dt;
        f.cc_height = lerp(f.cc_height, num, k);
        f.cc_center = lerp3(f.cc_center, Vec3::new(0.0, num / 2.0, 0.0), k);
    }
    // HandleSlide
    if !f.can_wall_jump {
        slide_fade_out(f, dt);
    }
    if f.is_crouch_slope_sliding {
        f.show_body_parts = true;
    }
    if f.slope_slide_trigger && f.is_crouch_slope_sliding {
        f.slope_slide_trigger = false;
        f.body_parts_pending = true;
    }
    if !f.is_crouching && mv == Vec2::ZERO {
        return;
    }
    if f.slide_timer > 0.0
        && f.slide_down_ray
        && f.move_direction.y < 0.0
        && f.is_crouching
        && f.is_slide_sprinting
    {
        f.slide_clip = 0;
        f.is_sliding = true;
        f.slide_timer = 0.0;
        f.slide_cancel_timer = -1.0;
        f.slide_audio_volume = 1.0;
        if !f.sprint_slide_trigger {
            f.slide_audio_playing = true;
        }
        f.body_parts_pending = true;
        if f.is_sprinting {
            f.sprint_slide = true;
            f.walk_slide = false;
        } else {
            f.sprint_slide = false;
            f.walk_slide = true;
        }
        let fwd = tf.forward().as_vec3();
        let (imp, dur) = if f.is_sprinting {
            (f.p.sprint_slide_impulsion, f.p.sprint_slide_duration)
        } else {
            (f.p.walk_slide_impulsion, f.p.walk_slide_duration)
        };
        f.add_horizontal_force(imp * fwd, dur);
    }
    if f.force_factor < 0.1 && f.is_sliding && f.gamepad {
        end_slide(f);
    }
    if f.force_factor == 0.0 || !f.slide_down_ray {
        f.is_sliding = false;
        f.sprint_slide = false;
        f.walk_slide = false;
        f.slide_cancel_timer = 2.0;
        if !f.is_crouch_slope_sliding {
            f.show_body_parts = false;
        }
    }
    f.slide_time -= dt;
    if f.is_sliding && f.slide_time < 0.0 {
        f.slide_time = 0.2;
        // SpawnVFX(position + forward +/- right, identity) -> Instantiate(slideDust) (RunLocally, so the owner sees it).
        let (fwd, right) = (tf.forward().as_vec3(), tf.right().as_vec3());
        f.slide_dust.push(pos + fwd + right);
        f.slide_dust.push(pos + fwd - right);
    }
}

fn handle_footsteps(
    f: &mut Fpc,
    ctx: &RapierContext,
    tf: &Transform,
    dt: f32,
    tags: &Query<&UnityTag>,
    sounds: &mut MessageWriter<FpcSound>,
) {
    if !f.cc_grounded
        || f.is_sliding
        || f.current_input_raw == Vec2::ZERO
        || f.player_speed < 1.0
        || f.is_leaning
        || f.is_crouching
    {
        return;
    }
    f.footstep_timer -= dt * f.movement_factor;
    if f.footstep_timer > 0.0 {
        return;
    }
    if let Some(h) = physics::raycast(
        ctx,
        tf.translation,
        Vec3::NEG_Y,
        3.0,
        physics::DEFAULT_RAYCAST_LAYERS,
    ) {
        let tag = tag_of(h.entity, tags);
        let idx = match tag.as_str() {
            "Footsteps/Concrete/Default" => 1,
            "Footsteps/Concrete/Dalles" => 2,
            "Footsteps/Concrete/Solide" => 3,
            "Footsteps/Concrete/PleinAir" => 20,
            "Footsteps/Wood/Creux" => 4,
            "Footsteps/Dirt" => 5,
            "Footsteps/Sand" => 6,
            "Footsteps/Grass" => 7,
            "Footsteps/Graviers" => 8,
            "Footsteps/Water" => 9,
            "Footsteps/Metal/Pipe2" => 12,
            "Footsteps/Metal/Grille" => 19,
            "Footsteps/Metal/Pipe" => 13,
            "Footsteps/Matelas" => 14,
            "Footsteps/Moquette" => 15,
            "Footsteps/Wood/Sec" => 16,
            _ => 1,
        };
        sounds.write(FpcSound::Indexed {
            index: idx,
            ground_tag: tag,
        });
    }
    f.footstep_timer = f.get_current_offset();
}

fn handle_bforce(f: &mut Fpc, dt: f32) {
    if f.bfactor > 0.0 {
        f.bforcefinal = Vec3::new(f.bdirection.x, 0.0, f.bdirection.z) * f.bfactor;
        f.bfactor -= dt * f.bdecel;
    }
    if f.bfactor < 0.0 {
        f.bforcefinal = Vec3::ZERO;
    }
    if f.bfactor <= 0.3 {
        f.baircontrol = true;
    }
    if f.bstop {
        f.bstop = false;
        f.bdirection = Vec3::ZERO;
        f.bfactor = 0.0;
        f.bforcefinal = Vec3::ZERO;
    }
    if f.bground_stop && f.cc_grounded && f.btimer > 0.5 {
        f.baircontrol = true;
        f.bdirection = Vec3::ZERO;
        f.bfactor = 0.0;
        f.bforcefinal = Vec3::ZERO;
        f.bground_stop = false;
    }
    f.btimer += dt;
}

fn handle_adding_force(f: &mut Fpc, dt: f32) {
    if f.initforce_factor > 20.0 && f.tempforce_speed < 10.0 {
        f.tempforce_speed = 10.0;
    } else if f.tempforce_speed < 2.0 {
        f.tempforce_speed = 2.0;
    }
    if !f.cc_grounded {
        if f.force_factor < 0.0 {
            f.force_factor = 0.0;
        }
        if f.force_factor > 0.0 {
            f.tempforce_speed -= dt * f.initforce_factor / 2.0;
        }
    } else {
        f.tempforce_speed = f.p.force_speed;
    }
    f.force_factor -= dt * (f.tempforce_speed * f.deceleration);
    if f.force_factor > 0.0 {
        f.adding_force = true;
        f.force_added = f.force * f.force_factor;
    } else {
        f.adding_force = false;
        f.force_added = Vec3::ZERO;
        f.force_factor = 0.0;
        f.force = Vec3::ZERO;
    }
    if f.force_factor <= 1.0 {
        f.deceleration = 1.0;
    }
}

fn handle_taunt(f: &mut Fpc, input: &PlayerInput, dt: f32, sounds: &mut MessageWriter<FpcSound>) {
    f.taunt_timer -= dt;
    if f.taunt_timer > 0.0 {
        return;
    }
    // Per-clip cooldowns for keys 1..0.
    const COOLDOWN: [f32; 10] = [0.4, 0.3, 0.3, 0.5, 0.7, 0.4, 0.7, 0.9, 1.0, 0.3];
    for i in 0..10 {
        if input.taunt[i] {
            sounds.write(FpcSound::Taunt(i as u8));
        }
    }
    for i in 0..10 {
        if input.taunt[i] {
            f.taunt_timer = COOLDOWN[i];
        }
    }
}

fn apply_final_movements(
    f: &mut Fpc,
    ctx: &RapierContext,
    tf: &Transform,
    mv: Vec2,
    dt: f32,
    rig: &mut ParamSet<RigQueries>,
    sounds: &mut MessageWriter<FpcSound>,
) -> Vec3 {
    let p = f.p.clone();
    // fall wind audio + high-fall VFX / lens distortion
    if f.player_speed < 15.0 {
        f.activate_fall_audio = true;
    }
    if f.player_speed > 15.0 && f.activate_fall_audio {
        f.activate_fall_audio = false;
        f.fall_audio_playing = true;
    }
    if f.player_speed > 15.0 && f.fall_distance > 4.0 {
        f.fall_audio_volume = lerp(f.fall_audio_volume, 1.0, p.fall_audio_lerp_speed * dt);
    } else {
        f.fall_audio_volume = lerp(f.fall_audio_volume, 0.0, p.fall_audio_lerp_out_speed * dt);
    }
    let falling_fx = f.rotation_x > 20.0 && f.fall_audio_volume > 0.05;
    f.high_fall_vfx_alpha = if falling_fx {
        lerp(f.high_fall_vfx_alpha, 255.0, 50.0 * dt)
    } else {
        lerp(f.high_fall_vfx_alpha, 0.0, 50.0 * dt)
    };
    f.lens_distortion = if falling_fx {
        lerp(
            f.lens_distortion,
            p.lens_distortion_when_falling,
            p.lens_distortion_lerp_speed * dt,
        )
    } else {
        lerp(
            f.lens_distortion,
            0.0,
            p.lens_distortion_lerp_out_speed * dt,
        )
    };
    if f.fall_audio_volume < 0.03 && f.player_speed < 15.0 {
        f.fall_audio_playing = false;
    }
    let slope_force = if mv != Vec2::ZERO {
        p.jump_slope_force
    } else {
        7.0
    };
    if f.prejump && f.cc_grounded && f.move_direction.y < 0.0 {
        f.all_jumped = true;
        f.slope_slide_jumped = true;
        f.move_direction.y = p.jump_force * f.jump_factor;
        f.anim_jump = true; // networkAnimator.SetTrigger("Jump")
        if f.is_sliding_steep {
            let n = f.hit_point_normal;
            f.b_force(n, slope_force, false, true, p.jump_slope_decel, true);
            punch_camera(
                rig,
                -p.fall_shake_force,
                p.fall_shake_duration,
                p.fall_shake_vibrato,
                p.fall_shake_elasticity,
            );
        }
        sounds.write(FpcSound::Indexed {
            index: 10,
            ground_tag: f.jump_tag.clone(),
        });
        f.slide_cancel_timer = 2.0;
        f.prejump = false;
        f.prejump_land = true;
    }
    if f.slide_end && f.cc_grounded {
        f.slide_end = false;
        f.deceleration = 2.5;
    }
    if f.move_direction.y < -3.0 && !f.cc_grounded {
        f.land_bool = true;
    }
    if f.land_bool && f.cc_grounded && !f.is_sliding_steep {
        f.land_bool = false;
        if !f.prejump_land {
            sounds.write(FpcSound::Indexed {
                index: 11,
                ground_tag: f.jump_tag.clone(),
            });
            if let Ok((bob, mut tw)) = rig.p1().single_mut() {
                if !f.is_crouching && bob.translation.y > -0.4 {
                    let to = bob.translation - Vec3::Y * p.land_bob_amount;
                    tw.local_move(to, p.land_bob_duration, p.land_bob_ease);
                }
            }
        }
        if f.fall_distance > p.fall_shake_height && !f.vault {
            if f.fall_distance > p.high_fall_shake_height {
                if let Ok((cam, mut tw, _)) = rig.p3().single_mut() {
                    let e = to_unity_euler(cam.rotation) - p.high_fall_shake_force;
                    tw.local_rotate(e, p.high_fall_shake_duration, Ease::OutQuad);
                }
                f.cam_rotate_back = true;
                f.cam_base_speed = p.high_fall_shake_recover_speed;
                sounds.write(FpcSound::Indexed {
                    index: 21,
                    ground_tag: String::new(),
                });
            } else {
                punch_camera(
                    rig,
                    -p.fall_shake_force,
                    p.fall_shake_duration,
                    p.fall_shake_vibrato,
                    p.fall_shake_elasticity,
                );
            }
        }
        f.footstep_timer = f.get_current_offset();
        f.prejump_land = false;
        f.fall_distance = 0.0;
    }
    if let Ok((mut bob, _)) = rig.p1().single_mut() {
        // LandBobPivot rests at its prefab local position (0, 0.1, 0); Unity lerps it to zero.
        if bob.translation.y < -0.02 {
            let k = p.land_recover_speed
                * dt
                * if f.is_crouching {
                    7.0
                } else if f.is_grounded {
                    1.0
                } else {
                    5.0
                };
            bob.translation = lerp3(bob.translation, Vec3::ZERO, k);
        }
    }
    if f.cc_grounded {
        f.land_timer = 0.3;
    }
    if f.ground_sphere_cast
        && angle(Vec3::Y, f.ground_normal) > 65.0
        && !f.jump_slope
        && f.is_grounded
    {
        f.bforcefinal = Vec3::ZERO;
        f.force_added = Vec3::ZERO;
    }
    let pos = tf.translation;
    let on_slope = |len: f32| -> bool {
        physics::raycast(ctx, pos, Vec3::NEG_Y, len, physics::DEFAULT_RAYCAST_LAYERS)
            .map(|h| {
                h.normal != Vec3::Y && {
                    let a = angle(h.normal, Vec3::Y);
                    a > 10.0 && a < 65.0
                }
            })
            .unwrap_or(false)
    };
    let grav = |f: &Fpc, slope: bool| -> f32 {
        let base = if f.is_crouching {
            if slope {
                p.crouch_gravity * p.slope_force * 2.0
            } else {
                p.crouch_gravity
            }
        } else if f.move_direction.y > 0.0 {
            p.jump_gravity
        } else if slope {
            p.gravity * p.slope_force
        } else {
            p.gravity
        };
        base * f.gravity_multiplier * dt
    };
    if f.safe_grounded && on_slope(p.slope_force_ray_length * 10.0) && f.on_ice && !f.jump_slope {
        f.move_direction.y -= grav(f, true);
    } else if f.safe_grounded
        && on_slope(p.slope_force_ray_length)
        && f.current_input.length() != 0.0
        && !f.jump_slope
    {
        f.move_direction.y -= grav(f, true);
    } else if !f.cc_grounded && f.move_direction.y > -40.0 {
        f.move_direction.y -= grav(f, false);
    }
    let total = f.move_direction
        + f.force_added
        + f.bforcefinal
        + f.move_added
        + f.slope_slide_move
        + f.steep_slope_slide_move
        + f.object_collision_move_direction
        + f.custom_force_final;
    if std::env::var_os("RESTRAFE_DEBUG_MOVE").is_some() && !f.cc_grounded {
        println!("MOVE md={:.2?} force={:.2?} bforce={:.2?} added={:.2?} slope={:.2?} steep={:.2?} obj={:.2?} custom={:.2?} speedFactor={:.2} wjc={}", f.move_direction, f.force_added, f.bforcefinal, f.move_added, f.slope_slide_move, f.steep_slope_slide_move, f.object_collision_move_direction, f.custom_force_final, f.speed_factor, f.wall_jumps_count);
    }
    f.jump_slope = false;
    total * dt
}

/// Applies FOV, lean/bob state and `CameraShakeConstrains` after tweens ran.
pub fn camera_post_update(
    time: Res<Time>,
    players: Query<&Fpc>,
    mut cams: Query<(&mut Transform, &mut Projection), With<PlayerCamera>>,
) {
    let Ok(f) = players.single() else { return };
    let Ok((mut tf, mut proj)) = cams.single_mut() else {
        return;
    };
    if let Projection::Perspective(pp) = &mut *proj {
        pp.fov = f.fov.to_radians();
    }
    if f.cam_rotate_back {
        // Quaternion.Euler(q.x, q.y, 0) with raw quaternion components (as in the original).
        let q = tf.rotation;
        let goal = unity_euler(-q.x, -q.y, 0.0);
        tf.rotation = slerp(tf.rotation, goal, f.cam_base_speed * time.delta_secs());
    }
}

/// Emits the queued slide dust (`FirstPersonController.slideDust` = VFX_Slide_00, Quaternion.identity).
pub fn slide_dust_fx(mut q: Query<&mut Fpc>, mut fx: MessageWriter<crate::weapons::gun::GunFx>) {
    for mut f in &mut q {
        for at in f.slide_dust.drain(..) {
            fx.write(crate::weapons::gun::GunFx::Spawn { prefab: Some("GameObject/VFX_Slide_00.prefab".into()), at, normal: Vec3::ZERO });
        }
    }
}

/// `highFallVfx` (PlayerIK Camera/VFX_SpeedParticle_Cam_00): a looping camera-child particle system whose
/// `startColor` is set every frame to Color(255, 255, 255, high_fall_vfx_alpha) (Color32 -> clamped to 0..1).
pub fn high_fall_vfx(
    mut vfx: crate::vfx::Vfx,
    cams: Query<(Entity, &GlobalTransform), With<PlayerCamera>>,
    players: Query<&Fpc>,
    mut roots: Query<&mut crate::vfx::sim::FxRoot>,
    mut root: Local<Option<Entity>>,
) {
    const PATH: &str = "PlayerIK/CameraLeanPivot/LandBobPivot/CameraHolder/Camera/VFX_SpeedParticle_Cam_00";
    let Ok((cam, cam_gt)) = cams.single() else { return };
    match *root {
        None => {
            let def = match crate::vfx::def::PrefabDef::load_sub("assets/data/player/PlayerIK.json", Some(PATH)) {
                Ok(d) if !d.objects.is_empty() => d,
                _ => {
                    *root = Some(Entity::PLACEHOLDER);
                    return;
                }
            };
            let local = Mat4::from_rotation_translation(def.objects[0].rot, def.objects[0].pos);
            let (_, rot, pos) = (cam_gt.to_matrix() * local).to_scale_rotation_translation();
            vfx.register("highFallVfx", def);
            let opts = crate::vfx::SpawnOpts { parent: Some((cam, cam_gt.to_matrix())), persistent: true, start_color: Some(Vec4::new(1.0, 1.0, 1.0, 0.0)), ..default() };
            *root = Some(vfx.spawn("highFallVfx", pos, rot, opts).unwrap_or(Entity::PLACEHOLDER));
        }
        Some(r) => {
            let (Ok(f), Ok(mut fx)) = (players.single(), roots.get_mut(r)) else { return };
            fx.start_color = Some(Vec4::new(1.0, 1.0, 1.0, f.high_fall_vfx_alpha.clamp(0.0, 1.0)));
        }
    }
}

/// FirstPersonController.HandleAnimation (owner): the third-person Animator of the local body, which the owner sees
/// only as the torso / legs while sliding. Hold bools as PlayerPickup sets them (default hold poses).
pub fn local_body_anim(
    mut players: Query<(&mut Fpc, &crate::weapons::pickup::Hands)>,
    lean: Query<&Transform, With<LeanCamera>>,
    items: Query<&crate::weapons::Item>,
    mut bodies: Query<&mut crate::animator::DummyAnimParams, With<crate::health::LocalBody>>,
) {
    let Ok((mut f, hands)) = players.single_mut() else { return };
    let Ok(mut p) = bodies.single_mut() else { return };
    let lean_z = lean.single().map(|t| crate::unity::to_unity_euler(t.rotation).z).unwrap_or(0.0);
    // FPC.Update: HandleAnimation only runs while SyncAccessor_canMove (parameters keep their last values otherwise).
    if f.can_move {
        p.crouch = f.is_crouching;
        p.grounded = f.slide_down_ray && f.move_direction.y <= 0.0;
        p.slide = f.is_sliding || f.is_crouch_slope_sliding || f.slide_timer > 0.1;
        p.crouch_move = f.last_input.0.length(); // move.ReadValue<Vector2>().magnitude (0..1)
        p.movement_speed = if f.is_sprinting { 1.0 } else if f.is_walking { 0.5 } else { 0.0 };
        p.vertical = if f.is_sliding { -(f.rotation_x / 90.0 - 1.0) - 2.0 } else if !f.is_sprinting { -f.rotation_x / 90.0 } else { -f.rotation_x / 90.0 + 0.3 };
        p.lean_right = f.is_leaning_right && !f.is_sliding && f.is_grounded && lean_z.abs() < 339.0;
        p.lean_left = f.is_leaning_left && !f.is_sliding && f.is_grounded && lean_z.abs() > 19.0;
    }
    let def = |e: Option<Entity>| e.and_then(|e| items.get(e).ok()).map(|i| i.def.clone());
    let (r, l) = (def(hands.right), def(hands.left));
    // PlayerPickup.Update (globalAnimator): right only -> TwoHanded / SingleHanded by requireBothHands; both hands ->
    // DoubleSingle; left only -> LeftHanded (requireBothHands not checked). rightHandAnim only affects the FP arms.
    let (rb, lb) = (r.is_some(), l.is_some());
    let rbh = r.as_ref().is_some_and(|d| d.require_both_hands());
    p.two_handed = rb && !lb && rbh;
    p.single_handed = rb && !lb && !rbh;
    p.double_single = rb && lb;
    p.left_handed = !rb && lb;
    if std::mem::take(&mut f.anim_jump) {
        p.jump = true; // networkAnimator.SetTrigger("Jump")
    }
}
