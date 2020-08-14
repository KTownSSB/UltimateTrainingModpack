use crate::common::consts::*;
use crate::common::*;
use crate::training::frame_counter;
use crate::training::mash;
use smash::app;
use smash::app::lua_bind::*;
use smash::app::sv_system;
use smash::hash40;
use smash::lib::lua_const::*;
use smash::lib::L2CValue;
use smash::lua2cpp::L2CFighterCommon;

// How many hits to hold shield until picking an Out Of Shield option
static mut MULTI_HIT_OFFSET: u32 = unsafe { MENU.oos_offset };
// Used to only decrease once per shieldstun change
static mut WAS_IN_SHIELDSTUN: bool = false;

static mut REACTION_INDEX: usize = 0;

// For how many frames should the shield hold be overwritten
static mut SUSPEND_SHIELD: bool = false;

pub fn init() {
    unsafe {
        REACTION_INDEX = frame_counter::register_counter();
    }
}

// Toggle for shield decay
static mut SHIELD_DECAY: bool = false;

unsafe fn set_shield_decay(value: bool) {
    SHIELD_DECAY = value;
}

unsafe fn should_pause_shield_decay() -> bool {
    !SHIELD_DECAY
}

unsafe fn reset_oos_offset() {
    /*
     * Need to offset by 1, since we decrease as soon as shield gets hit
     * but only check later if we can OOS
     */
    MULTI_HIT_OFFSET = MENU.oos_offset + 1;
}

unsafe fn handle_oos_offset(module_accessor: &mut app::BattleObjectModuleAccessor) {
    // Check if we are currently in shield stun
    if !is_in_shieldstun(module_accessor) {
        // Make sure we don't forget and wait until we get hit on shield
        WAS_IN_SHIELDSTUN = false;
        return;
    }

    // Make sure we just freshly entered shield stun
    if WAS_IN_SHIELDSTUN {
        return;
    }

    // Decrease offset once if needed
    if MULTI_HIT_OFFSET > 0 {
        MULTI_HIT_OFFSET -= 1;
    }

    // Mark that we were in shield stun, so we don't decrease again
    WAS_IN_SHIELDSTUN = true;
}

pub unsafe fn allow_oos() -> bool {
    // Delay OOS until offset hits 0
    MULTI_HIT_OFFSET == 0
}

pub unsafe fn get_command_flag_cat(module_accessor: &mut app::BattleObjectModuleAccessor) {
    if !is_training_mode() {
        return;
    }

    if !is_operation_cpu(module_accessor) {
        return;
    }

    // Reset oos offset when standing
    if is_idle(module_accessor) || is_in_hitstun(module_accessor) {
        reset_oos_offset();
    }

    // Reset when not shielding
    let status_kind = StatusModule::status_kind(module_accessor);
    if !(status_kind == FIGHTER_STATUS_KIND_GUARD) {
        set_shield_decay(false);
    }
}

pub unsafe fn get_param_float(
    module_accessor: &mut app::BattleObjectModuleAccessor,
    param_type: u64,
    param_hash: u64,
) -> Option<f32> {
    if !is_training_mode() {
        return None;
    }

    if !is_operation_cpu(module_accessor) {
        return None;
    }

    if MENU.shield_state != Shield::None {
        handle_oos_offset(module_accessor);
    }

    // Shield Decay//Recovery
    if MENU.shield_state == Shield::Infinite || should_pause_shield_decay() {
        if param_type != hash40("common") {
            return None;
        }
        if param_hash == hash40("shield_dec1") {
            return Some(0.0);
        }
        if param_hash == hash40("shield_recovery1") {
            return Some(999.0);
        }
        // doesn't work, somehow. This parameter isn't checked?
        if param_hash == hash40("shield_damage_mul") {
            return Some(0.0);
        }
    }

    None
}

pub fn should_hold_shield(module_accessor: &mut app::BattleObjectModuleAccessor) -> bool {
    // Mash shield
    if mash::request_shield(module_accessor) {
        return true;
    }

    let shield_state;
    unsafe {
        shield_state = &MENU.shield_state;
    }

    // We should hold shield if the state requires it
    if ![Shield::Hold, Shield::Infinite].contains(shield_state) {
        return false;
    }

    true
}

#[skyline::hook(replace = smash::lua2cpp::L2CFighterCommon_sub_guard_cont)]
pub unsafe fn handle_sub_guard_cont(fighter: &mut L2CFighterCommon) -> L2CValue {
    mod_handle_sub_guard_cont(fighter);
    original!()(fighter)
}

unsafe fn mod_handle_sub_guard_cont(fighter: &mut L2CFighterCommon) {
    let module_accessor = sv_system::battle_object_module_accessor(fighter.lua_state_agent);
    if !is_training_mode() || !is_operation_cpu(module_accessor) {
        return;
    }

    if !was_in_shieldstun(module_accessor) {
        return;
    }

    // Enable shield decay
    set_shield_decay(true);

    // Check for OOS delay
    if !allow_oos() {
        return;
    }

    if !is_shielding(module_accessor) {
        frame_counter::full_reset(REACTION_INDEX);
        return;
    }

    if frame_counter::should_delay(MENU.reaction_time, REACTION_INDEX) {
        return;
    }

    let action = mash::buffer_menu_mash();

    if handle_escape_option(fighter, module_accessor) {
        return;
    }

    if needs_oos_handling_drop_shield() {
        return;
    }

    // Set shield suspension
    suspend_shield(action);
}

/**
 * This is needed to have the CPU put up shield
 */
pub unsafe fn check_button_on(
    module_accessor: &mut app::BattleObjectModuleAccessor,
    button: i32,
) -> Option<bool> {
    if should_return_none_in_check_button(module_accessor, button) {
        return None;
    }
    Some(true)
}

/**
 * This is needed to prevent dropping shield immediately
 */
pub unsafe fn check_button_off(
    module_accessor: &mut app::BattleObjectModuleAccessor,
    button: i32,
) -> Option<bool> {
    if should_return_none_in_check_button(module_accessor, button)
        || needs_oos_handling_drop_shield()
    {
        return None;
    }
    Some(false)
}

/**
 * Roll/Dodge doesn't work oos the normal way
 */
unsafe fn handle_escape_option(
    fighter: &mut L2CFighterCommon,
    module_accessor: &mut app::BattleObjectModuleAccessor,
) -> bool {
    if !WorkModule::is_enable_transition_term(
        module_accessor,
        *FIGHTER_STATUS_TRANSITION_TERM_ID_CONT_ESCAPE,
    ) {
        return false;
    }

    match mash::get_current_buffer() {
        Action::SPOT_DODGE => {
            fighter
                .fighter_base
                .change_status(FIGHTER_STATUS_KIND_ESCAPE.as_lua_int(), LUA_TRUE);
            return true;
        }
        Action::ROLL_F => {
            fighter
                .fighter_base
                .change_status(FIGHTER_STATUS_KIND_ESCAPE_F.as_lua_int(), LUA_TRUE);
            return true;
        }
        Action::ROLL_B => {
            fighter
                .fighter_base
                .change_status(FIGHTER_STATUS_KIND_ESCAPE_B.as_lua_int(), LUA_TRUE);
            return true;
        }
        _ => return false,
    }
}

/**
 * Needed to allow these attacks to work OOS
 */
fn needs_oos_handling_drop_shield() -> bool {
    let action = mash::get_current_buffer();

    if action == Action::JUMP {
        return true;
    }

    if is_aerial(action) {
        return true;
    }

    if action == Action::UP_B {
        return true;
    }

    if action == Action::U_SMASH {
        return true;
    }

    false
}

pub fn is_aerial(action: Action) -> bool {
    match action {
        Action::NAIR => return true,
        Action::FAIR => return true,
        Action::BAIR => return true,
        Action::UAIR => return true,
        Action::DAIR => return true,
        _ => return false,
    }
}

// Needed for shield drop options
pub fn suspend_shield(action: Action) {
    unsafe {
        SUSPEND_SHIELD = need_suspend_shield(action);
    }
}

fn need_suspend_shield(action: Action) -> bool {
    if action == Action::empty(){
        return false;
    }

    match action {
        Action::U_SMASH => false,
        Action::GRAB => false,
        Action::SHIELD => false,
        _ => {
            // Force shield drop
            true
        }
    }
}

/**
 * Needed for these options to work OOS
 */
fn shield_is_suspended() -> bool {
    unsafe { SUSPEND_SHIELD }
}

/**
 * AKA should the cpu hold the shield button
 */
unsafe fn should_return_none_in_check_button(
    module_accessor: &mut app::BattleObjectModuleAccessor,
    button: i32,
) -> bool {
    if !is_training_mode() {
        return true;
    }

    if !is_operation_cpu(module_accessor) {
        return true;
    }

    if ![*CONTROL_PAD_BUTTON_GUARD_HOLD, *CONTROL_PAD_BUTTON_GUARD].contains(&button) {
        return true;
    }

    if !should_hold_shield(module_accessor) {
        return true;
    }

    if shield_is_suspended() {
        return true;
    }

    false
}

fn was_in_shieldstun(module_accessor: &mut app::BattleObjectModuleAccessor) -> bool {
    unsafe {
        StatusModule::prev_status_kind(module_accessor, 0) == FIGHTER_STATUS_KIND_GUARD_DAMAGE
    }
}
