use crate::{
    lang,
    model::{self, Afk, Config, Toggle},
};
use sp_travel::runtime::{find, here, id};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};
use strawberry_plugin_api::{
    Context, EventHandler, Player, Plugin, PluginMetadata, Result, Server,
    command::{Command, CommandError, CommandSender, ConsumedArgs},
    commands::CommandHandler,
    common::NamedColor,
    events::{FailureVerdict, HandlerOptions, HandlerPriority, PlayerChatEvent, PlayerLeaveEvent},
    events_wit::{PlayerChatEventData, PlayerLeaveEventData},
    permission::{Permission, PermissionDefault},
    permissions, register_plugin,
    scheduler::{self, SchedulerExt},
    text::TextComponent,
};

const USE: &str = "afk-lite:use";
type Shared = Arc<Mutex<State>>;

struct State {
    afk: Afk,
    config: Config,
    clock: Instant,
    /// The one task that looks at the players who are away. `None` while nobody is.
    timer: Option<u32>,
    running: bool,
}

fn lock(state: &Shared) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn component(locale: &str, key: &str, args: &[&str]) -> TextComponent {
    TextComponent::text(&lang::text(locale, key, args))
}

/// Tells the player and everybody who can see him. A player hidden from a viewer (a staff script's vanish)
/// is not announced to that viewer.
fn announce(server: &Server, player: &Player, away: bool) {
    let (own, others) = if away {
        ("you_afk", "now_afk")
    } else {
        ("you_back", "no_longer_afk")
    };
    player.send_system_message(
        component(&player.get_locale(), own, &[]).color_named(NamedColor::Gray),
        false,
    );
    let me = id(player);
    let name = player.get_name();
    for viewer in server.get_all_players() {
        if id(&viewer) == me {
            continue;
        }
        let Some(target) = find(server, me) else {
            return;
        };
        if viewer.can_see_player(target) {
            viewer.send_system_message(
                component(&viewer.get_locale(), others, &[&name]).color_named(NamedColor::Gray),
                false,
            );
        }
    }
}

/// Leaves the player who just went away out of the players who must sleep for the night to pass, when the
/// setting asks for it. The server keeps this wish apart from any other plugin's.
fn leave_out_of_sleep(shared: &Shared, player: &Player) {
    let ignore = {
        let mut state = lock(shared);
        state.config.sleep_ignores_afk && state.afk.ignore(id(player))
    };
    if ignore {
        player.set_sleeping_ignored(true);
    }
}

/// Counts the player who is back, or leaving, for the night's sleep again, only if this script left him out:
/// a wish another plugin made for him stays as it is.
fn count_for_sleep(shared: &Shared, player: &Player) {
    if lock(shared).afk.release(id(player)) {
        player.set_sleeping_ignored(false);
    }
}

/// Starts the look at the players who are away, if somebody is and it does not run yet.
fn arm(shared: &Shared, server: &Server) {
    let ticks = {
        let state = lock(shared);
        if state.timer.is_some() || !state.running || !state.afk.any() {
            return;
        }
        state.config.check_seconds.saturating_mul(20)
    };
    let weak = Arc::downgrade(shared);
    let task = server.schedule_delayed_task(ticks, move |server| {
        let Some(shared) = weak.upgrade() else {
            return;
        };
        let away = {
            let mut state = lock(&shared);
            state.timer = None;
            state.afk.away()
        };
        for (key, at) in away {
            let Some(player) = find(&server, key) else {
                // Gone: the server counts him again at his next join.
                let mut state = lock(&shared);
                state.afk.back(key);
                state.afk.release(key);
                continue;
            };
            let world = player.get_world().get_name();
            if sp_travel::moved(&at, &world, player.get_position()) && lock(&shared).afk.back(key) {
                count_for_sleep(&shared, &player);
                announce(&server, &player, false);
            }
        }
        arm(&shared, &server);
    });
    lock(shared).timer = Some(task);
}

/// The look stops with the last player who was away.
fn disarm(shared: &Shared) {
    let task = {
        let mut state = lock(shared);
        if state.afk.any() {
            None
        } else {
            state.timer.take()
        }
    };
    if let Some(task) = task {
        scheduler::cancel_task(task);
    }
}

struct Run(Shared);

impl CommandHandler for Run {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        _args: ConsumedArgs,
    ) -> std::result::Result<i32, CommandError> {
        let Some(player) = sender.as_player() else {
            return Err(CommandError::CommandFailed(component(
                "en",
                "players_only",
                &[],
            )));
        };
        let refuse = |key: &str, args: &[&str]| {
            CommandError::CommandFailed(
                component(&player.get_locale(), key, args).color_named(NamedColor::Red),
            )
        };
        if !player.has_permission(USE) {
            return Err(refuse("no_permission", &[]));
        }
        let place = here(&player);
        let toggle = {
            let mut state = lock(&self.0);
            if !state.running {
                return Err(refuse("busy", &[]));
            }
            let now = u64::try_from(state.clock.elapsed().as_millis()).unwrap_or(u64::MAX);
            let cooldown = state.config.cooldown_seconds;
            state.afk.toggle(id(&player), place, now, cooldown)
        };
        match toggle {
            Toggle::Away => {
                leave_out_of_sleep(&self.0, &player);
                announce(&server, &player, true);
                arm(&self.0, &server);
                Ok(1)
            }
            Toggle::Back => {
                count_for_sleep(&self.0, &player);
                disarm(&self.0);
                announce(&server, &player, false);
                Ok(1)
            }
            Toggle::Wait(seconds) => Err(refuse("cooldown", &[&seconds.to_string()])),
            Toggle::Full => Err(refuse("busy", &[])),
        }
    }
}

struct Events(Shared);

impl Events {
    fn back(&self, player: &Player) -> bool {
        let was = lock(&self.0).afk.back(id(player));
        if was {
            count_for_sleep(&self.0, player);
            disarm(&self.0);
        }
        was
    }
}

impl EventHandler<PlayerChatEvent> for Events {
    fn handle(&self, server: Server, event: PlayerChatEventData) -> PlayerChatEventData {
        // A player who writes is there, also when the line itself was cancelled: a chat format cancels every
        // line it replaces, so a cancelled line says nothing about its writer.
        if self.back(&event.player) {
            announce(&server, &event.player, false);
        }
        event
    }
}

impl EventHandler<PlayerLeaveEvent> for Events {
    fn handle(&self, _server: Server, event: PlayerLeaveEventData) -> PlayerLeaveEventData {
        // The quit line says he is gone; no second line.
        self.back(&event.player);
        event
    }
}

struct AfkLite {
    state: Shared,
}

impl Plugin for AfkLite {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                afk: Afk::default(),
                config: Config {
                    cooldown_seconds: 60,
                    check_seconds: 2,
                    sleep_ignores_afk: true,
                },
                clock: Instant::now(),
                timer: None,
                running: false,
            })),
        }
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "afk-lite".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Strawberry Interactive LLC".into()],
            description: lang::text("en", "description", &[]),
            dependencies: vec![],
            permissions: vec![
                permissions::FS_READ_DATA.into(),
                permissions::FS_WRITE_DATA.into(),
            ],
        }
    }

    fn on_load(&self, context: Context) -> Result<()> {
        let dir = PathBuf::from(context.get_data_folder());
        std::fs::create_dir_all(&dir).map_err(|_| lang::text("en", "data_error", &[]))?;
        let config_path = dir.join("config.properties");
        let text = sp_travel::files::read_optional(&config_path, 4096)
            .map_err(|key| lang::text("en", key, &[]))?;
        let defaults = sp_travel::settings::default_text(&model::FIELDS);
        if text.is_none() {
            std::fs::write(&config_path, &defaults)
                .map_err(|_| lang::text("en", "save_error", &[]))?;
        }
        let config = Config::parse(text.as_deref().unwrap_or(&defaults))
            .map_err(|key| lang::text("en", key, &[]))?;
        {
            let mut state = lock(&self.state);
            state.config = config;
            state.running = true;
        }
        context.register_permission(&Permission {
            node: USE.into(),
            description: lang::text("en", "permission_use", &[]),
            default: PermissionDefault::Allow,
            children: vec![],
        })?;
        context.register_command(
            Command::new(
                &["afk".into(), "away".into()],
                &lang::text("en", "command_afk", &[]),
            )
            .execute(Run(Arc::clone(&self.state))),
            USE,
        );
        // Observers: they never change the chat line or the leave, and a fault in them stops nothing. A chat
        // format cancels the line it replaces and a hidden leave is a cancelled one: both still count here.
        let observe = HandlerOptions {
            priority: HandlerPriority::Monitor,
            blocking: true,
            ignore_cancelled: false,
            on_failure: FailureVerdict::Allow,
        };
        context.register_event_handler_with_options::<PlayerChatEvent, _>(
            Events(Arc::clone(&self.state)),
            observe,
        )?;
        context.register_event_handler_with_options::<PlayerLeaveEvent, _>(
            Events(Arc::clone(&self.state)),
            observe,
        )?;
        Ok(())
    }

    fn on_unload(&self, context: Context) -> Result<()> {
        let (task, left_out) = {
            let mut state = lock(&self.state);
            state.running = false;
            let left_out = state.afk.clear();
            (state.timer.take(), left_out)
        };
        if let Some(task) = task {
            scheduler::cancel_task(task);
        }
        // Everybody this script left out of the sleep count is counted again (the server also takes an unloaded
        // plugin's wishes back; this does not wait for it).
        if !left_out.is_empty() {
            let server = context.get_server();
            for key in left_out {
                if let Some(player) = find(&server, key) {
                    player.set_sleeping_ignored(false);
                }
            }
        }
        Ok(())
    }
}

register_plugin!(AfkLite);
