/*
- every frame server broadcasts position of all balls
  via unreliable channel server->client
- client sends movement commands to server
  via reliable channel client->server
*/

use bevy::{
    app::{stage, App, EventReader, Events, ScheduleRunnerPlugin},
    core::CorePlugin,
    prelude::*,
    render::{camera::WindowOrigin, pass::ClearColor},
    sprite::collide_aabb::{collide, Collision},
    type_registry::TypeRegistryPlugin,
};
use bevy_networking_turbulence::{
    ConnectionChannelsBuilder, MessageChannelMode, MessageChannelSettings, NetworkEvent,
    NetworkResource, NetworkingPlugin, ReliableChannelSettings,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, net::SocketAddr, time::Duration};

mod utils;
use utils::*;

const SERVER_PORT: u16 = 14192;
const BOARD_WIDTH: u32 = 1000;
const BOARD_HEIGHT: u32 = 1000;

fn main() {
    simple_logger::SimpleLogger::from_env()
        .init()
        .expect("A logger was already initialized");

    App::build().add_plugin(BallsExample).run();
}

struct Pawn {
    controller: u32,
}
struct Ball {
    velocity: Vec3,
}

struct BallsExample;

impl Plugin for BallsExample {
    fn build(&self, app: &mut AppBuilder) {
        let args = parse_args();
        if args.is_server {
            // Server
            app.add_plugin(TypeRegistryPlugin::default())
                .add_plugin(CorePlugin)
                .add_plugin(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(
                    1.0 / 60.0,
                )))
                .add_startup_system(server_setup.system())
                .add_system(ball_movement_system.system())
                .add_resource(NetworkBroadcast { frame: 0 })
                .add_system_to_stage(stage::PRE_UPDATE, handle_messages_server.system())
                .add_system_to_stage(stage::POST_UPDATE, network_broadcast_system.system())
        } else {
            // Client
            app.add_resource(WindowDescriptor {
                width: BOARD_WIDTH,
                height: BOARD_HEIGHT,
                ..Default::default()
            })
            .add_default_plugins()
            .add_resource(ClearColor(Color::rgb(0.3, 0.3, 0.3)))
            .add_startup_system(client_setup.system())
            .add_system_to_stage(stage::PRE_UPDATE, handle_messages_client.system())
            .add_resource(ServerIds::default())
        }
        .add_resource(args)
        .add_plugin(NetworkingPlugin)
        .add_startup_system(network_setup.system())
        .add_resource(NetworkReader::default())
        .add_system(handle_packets.system());
    }
}

fn ball_movement_system(time: Res<Time>, mut ball_query: Query<(&Ball, &mut Transform)>) {
    // clamp the timestep to stop the ball from escaping when the game starts
    let delta_seconds = f32::min(0.2, time.delta_seconds);

    for (ball, mut transform) in &mut ball_query.iter() {
        transform.translate(ball.velocity * delta_seconds);
        let translation = transform.translation_mut();
        let mut x = translation.x() as i32 % BOARD_WIDTH as i32;
        let mut y = translation.y() as i32 % BOARD_HEIGHT as i32;
        if x < 0 {
            x += BOARD_WIDTH as i32;
        }
        if y < 0 {
            y += BOARD_HEIGHT as i32;
        }
        translation.set_x(x as f32);
        translation.set_y(y as f32);
    }
}

fn server_setup(mut commands: Commands, mut net: ResMut<NetworkResource>) {
    let ip_address =
        bevy_networking_turbulence::find_my_ip_address().expect("can't find ip address");
    let socket_address = SocketAddr::new(ip_address, SERVER_PORT);
    log::info!("Starting server");
    net.listen(socket_address);
}

fn client_setup(mut commands: Commands, mut net: ResMut<NetworkResource>) {
    let mut camera = Camera2dComponents::default();
    camera.orthographic_projection.window_origin = WindowOrigin::BottomLeft;
    commands.spawn(camera);

    let ip_address =
        bevy_networking_turbulence::find_my_ip_address().expect("can't find ip address");
    let socket_address = SocketAddr::new(ip_address, SERVER_PORT);
    log::info!("Starting client");
    net.connect(socket_address);
}

fn network_setup(mut net: ResMut<NetworkResource>) {
    net.set_channels_builder(|builder: &mut ConnectionChannelsBuilder| {
        builder
            .register::<HelloMessage>(CLIENT_STATE_MESSAGE_SETTINGS)
            .unwrap();
        builder
            .register::<GameStateMessage>(GAME_STATE_MESSAGE_SETTINGS)
            .unwrap();
    });
}

#[derive(Default)]
struct NetworkBroadcast {
    frame: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct HelloMessage {
    id: String,
}

const CLIENT_STATE_MESSAGE_SETTINGS: MessageChannelSettings = MessageChannelSettings {
    channel: 0,
    channel_mode: MessageChannelMode::Reliable {
        reliability_settings: ReliableChannelSettings {
            bandwidth: 4096,
            recv_window_size: 1024,
            send_window_size: 1024,
            burst_bandwidth: 1024,
            init_send: 512,
            wakeup_time: Duration::from_millis(100),
            initial_rtt: Duration::from_millis(200),
            max_rtt: Duration::from_secs(2),
            rtt_update_factor: 0.1,
            rtt_resend_factor: 1.5,
        },
        max_message_len: 1024,
    },
    message_buffer_size: 8,
    packet_buffer_size: 8,
};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct GameStateMessage {
    frame: u32,
    balls: Vec<(u32, Vec3, Vec3)>,
}

const GAME_STATE_MESSAGE_SETTINGS: MessageChannelSettings = MessageChannelSettings {
    channel: 1,
    channel_mode: MessageChannelMode::Unreliable,
    message_buffer_size: 8,
    packet_buffer_size: 8,
};

fn network_broadcast_system(
    mut state: ResMut<NetworkBroadcast>,
    mut net: ResMut<NetworkResource>,
    mut ball_query: Query<(Entity, &Ball, &Transform)>,
) {
    let mut message = GameStateMessage {
        frame: state.frame,
        balls: Vec::new(),
    };
    state.frame += 1;

    for (entity, ball, transform) in &mut ball_query.iter() {
        message
            .balls
            .push((entity.id(), ball.velocity, transform.translation()));
    }

    net.broadcast_message(message);
}

#[derive(Default)]
struct NetworkReader {
    network_events: EventReader<NetworkEvent>,
}

fn handle_packets(
    mut commands: Commands,
    mut net: ResMut<NetworkResource>,
    mut state: ResMut<NetworkReader>,
    args: Res<Args>,
    network_events: Res<Events<NetworkEvent>>,
) {
    for event in state.network_events.iter(&network_events) {
        match event {
            NetworkEvent::Connected(handle) => match net.connections.get_mut(handle) {
                Some(connection) => {
                    match connection.remote_address() {
                        Some(remote_address) => {
                            log::debug!(
                                "Incoming connection on [{}] from [{}]",
                                handle,
                                remote_address
                            );

                            // New client connected - spawn a ball
                            let mut rng = rand::thread_rng();
                            commands.spawn((
                                Ball {
                                    velocity: 400.0
                                        * Vec3::new(
                                            rng.gen_range(-0.5, 0.5),
                                            rng.gen_range(-0.5, 0.5),
                                            0.0,
                                        )
                                        .normalize(),
                                },
                                Pawn {
                                    controller: *handle,
                                },
                                Transform::from_translation(Vec3::new(
                                    rng.gen_range(0.0, BOARD_WIDTH as f32),
                                    rng.gen_range(0.0, BOARD_HEIGHT as f32),
                                    1.0,
                                )),
                            ));
                        }
                        None => {
                            log::debug!("Connected on [{}]", handle);
                        }
                    }

                    if !args.is_server {
                        log::debug!("Sending Hello on [{}]", handle);
                        match net.send_message(
                            *handle,
                            HelloMessage {
                                id: "test".to_string(),
                            },
                        ) {
                            Ok(msg) => match msg {
                                Some(msg) => {
                                    log::error!("Unable to send Hello: {:?}", msg);
                                }
                                None => {}
                            },
                            Err(err) => {
                                log::error!("Unable to send Hello: {:?}", err);
                            }
                        };
                    }
                }
                None => panic!("Got packet for non-existing connection [{}]", handle),
            },
            _ => {}
        }
    }
}

fn handle_messages_server(mut net: ResMut<NetworkResource>) {
    for (handle, connection) in net.connections.iter_mut() {
        let channels = connection.channels().unwrap();
        while let Some(hello_message) = channels.recv::<HelloMessage>() {
            log::info!(
                "HelloMessage received on [{}]: {}",
                handle,
                hello_message.id
            );
            // TODO: store client id?
        }

        while let Some(_state_message) = channels.recv::<GameStateMessage>() {
            log::error!("GameStateMessage received on [{}]", handle);
        }
    }
}

type ServerIds = HashMap<u32, (u32, u32)>;

fn handle_messages_client(
    mut commands: Commands,
    mut net: ResMut<NetworkResource>,
    mut server_ids: ResMut<ServerIds>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut balls: Query<(Entity, &mut Ball, &mut Transform)>,
) {
    for (handle, connection) in net.connections.iter_mut() {
        let channels = connection.channels().unwrap();
        while let Some(_hello_message) = channels.recv::<HelloMessage>() {
            log::error!("HelloMessage received on [{}]", handle);
        }

        // it is possible that many state updates came at the same time - spawn once
        let mut to_spawn: HashMap<u32, (u32, Vec3, Vec3)> = HashMap::new();

        while let Some(mut state_message) = channels.recv::<GameStateMessage>() {
            let message_frame = state_message.frame;
            log::info!(
                "GameStateMessage received on [{}]: {:?}",
                handle,
                state_message
            );

            // update all balls
            for (entity, mut ball, mut transform) in &mut balls.iter() {
                let server_id_entry = server_ids.get_mut(&entity.id()).unwrap();
                let (server_id, update_frame) = *server_id_entry;

                if let Some(index) = state_message
                    .balls
                    .iter()
                    .position(|&update| update.0 == server_id)
                {
                    let (_id, velocity, translation) = state_message.balls.remove(index);

                    if update_frame > message_frame {
                        continue;
                    }
                    server_id_entry.1 = message_frame;

                    ball.velocity = velocity;
                    transform.set_translation(translation);
                } else {
                    // TODO: despawn disconnected balls
                }
            }
            // create new balls
            for (id, velocity, translation) in state_message.balls.drain(..) {
                if let Some((frame, _velocity, _translation)) = to_spawn.get(&id) {
                    if *frame > message_frame {
                        continue;
                    }
                };
                to_spawn.insert(id, (message_frame, velocity, translation));
            }
        }

        for (id, (frame, velocity, translation)) in to_spawn.iter() {
            log::info!("Spawning {} @{}", id, frame);
            let entity = commands
                .spawn(SpriteComponents {
                    material: materials.add(Color::rgb(0.8, 0.2, 0.2).into()),
                    transform: Transform::from_translation(*translation),
                    sprite: Sprite::new(Vec2::new(30.0, 30.0)),
                    ..Default::default()
                })
                .with(Ball {
                    velocity: *velocity,
                })
                .with(Pawn { controller: *id })
                .current_entity()
                .unwrap();
            server_ids.insert(entity.id(), (*id, *frame));
        }
    }
}
