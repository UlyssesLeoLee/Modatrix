use bevy::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "FrameForge 3D — Review Workspace".into(),
                resolution: (1280, 800).into(),
                ..default()
            }),
            ..default()
        }))
        .add_systems(Startup, setup_scene)
        .add_systems(Update, rotate_preview)
        .run();
}

#[derive(Component)]
struct PreviewMesh;

fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(3.0, 2.5, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 8_000.0,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.8, -0.5, 0.0)),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(1.0, 1.6, 0.5))),
        MeshMaterial3d(materials.add(Color::srgb(0.22, 0.48, 0.72))),
        Transform::default(),
        PreviewMesh,
    ));
}

fn rotate_preview(time: Res<Time>, mut query: Query<&mut Transform, With<PreviewMesh>>) {
    for mut transform in &mut query {
        transform.rotate_y(time.delta_secs() * 0.25);
    }
}
