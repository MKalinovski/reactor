use bevy::prelude::*;

use super::{schedule::RunSimulation, types::*};

pub(super) fn plugin(app: &mut App) {
    app.add_systems(
        RunSimulation,
        (
            update_local_reactivity,
            update_edge_reactivity,
            update_total_reactivity,
            update_temperature,
            generate_steam,
        )
            .chain(),
    );
    app.add_systems(
        RunSimulation,
        update_edge_temperatures.before(update_temperature),
    );
}

fn update_edge_reactivity(
    cores: Query<&ReactorCore>,
    mut edges: Query<(&mut Reactivity, &ReactorEdge, &ChildOf), Without<ReactorCell>>,
    cells: Query<&LocalReactivity, With<ReactorCell>>,
) -> Result {
    for (mut edge_reactivity, edge, child_of) in &mut edges {
        let core = cores.get(child_of.0)?;
        let Some(first) = core.cells_by_pos.get(&edge.nodes.0) else {
            warn!("First node not found");
            continue;
        };
        let Some(second) = core.cells_by_pos.get(&edge.nodes.1) else {
            warn!("Second node not found");
            continue;
        };
        let reactivities = [cells.get(*first)?.0, cells.get(*second)?.0];
        edge_reactivity.0 = (reactivities[0] + reactivities[1]) / 2.;
    }
    Ok(())
}

fn update_local_reactivity(
    config: Res<SimulationConfig>,
    mut query: Query<(&mut LocalReactivity, &ControlRod, &CoolantLevel), With<ReactorCell>>,
) {
    for (mut local_reactivity, control_rod, coolant_level) in &mut query {
        let rod_factor = 1.0 - control_rod.0; // control rods absorb
        let coolant_factor = 1.0 + config.void_reactivity_boost * (1.0 - coolant_level.0); // steam = more reactivity
        local_reactivity.0 = config.base_reactivity * rod_factor * coolant_factor;
    }
}

fn update_total_reactivity(
    config: Res<SimulationConfig>,
    cores: Query<&ReactorCore>,
    mut query: Query<
        (&mut Reactivity, &ReactorCell, &LocalReactivity, &ChildOf),
        Without<ReactorEdge>,
    >,
    edge_reactivities: Query<&Reactivity, (With<ReactorEdge>, Without<ReactorCell>)>,
) -> Result {
    for (mut reactivity, cell, local_reactivity, child_of) in &mut query {
        let core = cores.get(child_of.0)?;

        let mut neighbor_sum = 0.0;
        for pos in cell.0.neighbours() {
            neighbor_sum += match core.find_edge(cell.0, pos) {
                Some(edge) => edge_reactivities.get(edge)?.0,
                None => 0.0,
            }
        }

        let neighbor_boost = neighbor_sum * config.reactivity_neighbor_coupling_factor;
        reactivity.0 = local_reactivity.0 + neighbor_boost;
    }

    Ok(())
}

fn update_edge_temperatures(
    cores: Query<&ReactorCore>,
    mut edges: Query<(&mut Temperature, &ReactorEdge, &ChildOf), Without<ReactorCell>>,
    cells: Query<&Temperature, With<ReactorCell>>,
) -> Result {
    for (mut edge_temperature, edge, child_of) in &mut edges {
        let core = cores.get(child_of.0)?;
        let Some(first) = core.cells_by_pos.get(&edge.nodes.0) else {
            warn!("First node not found");
            continue;
        };
        let Some(second) = core.cells_by_pos.get(&edge.nodes.1) else {
            warn!("Second node not found");
            continue;
        };

        // Edge temperature becomes the average of connected cells
        let temps = [cells.get(*first)?.0, cells.get(*second)?.0];
        edge_temperature.0 = (temps[0] + temps[1]) / 2.;
    }
    Ok(())
}

fn update_temperature(
    config: Res<SimulationConfig>,
    cores: Query<&ReactorCore>,
    mut query: Query<
        (
            &mut Temperature,
            &ReactorCell,
            &ChildOf,
            &Reactivity,
            &CoolantLevel,
        ),
        Without<ReactorEdge>,
    >,
    edge_temperatures: Query<&Temperature, (With<ReactorEdge>, Without<ReactorCell>)>,
) -> Result {
    for (mut temperature, cell, child_of, reactivity, coolant_level) in &mut query {
        let core = cores.get(child_of.0)?;

        let mut neighbor_temp_sum = 0.0;
        for pos in cell.0.neighbours() {
            neighbor_temp_sum += match core.find_edge(cell.0, pos) {
                Some(edge) => edge_temperatures.get(edge)?.0,
                None => 25.0,
            }
        }

        let ambient_temperature = neighbor_temp_sum / 4.;
        let heat_gain = reactivity.0 * config.heat_generation_factor;
        let coolant_temp_diff = temperature.0 - config.coolant_temperature;
        let heat_loss = coolant_level.0 * config.coolant_efficiency * coolant_temp_diff;
        let ambient_temp_diff = temperature.0 - ambient_temperature;
        let passive_heat_loss = ambient_temp_diff * config.temperature_passive_decay_rate;

        temperature.0 += heat_gain - heat_loss - passive_heat_loss;
    }

    Ok(())
}

fn generate_steam(
    config: Res<SimulationConfig>,
    mut query: Query<
        (
            &mut SteamOutput,
            &mut CoolantLevel,
            &mut SteamLevel,
            &Temperature,
            &CoolantFlow,
            &mut Pressure,
            &SteamPullCapacity,
        ),
        With<ReactorCell>,
    >,
) {
    for (
        mut steam_output,
        mut coolant_level,
        mut steam_level,
        temperature,
        coolant_flow,
        mut pressure,
        steam_pull_capacity,
    ) in &mut query
    {
        // Boiling point of water depends on pressure (roughly 3 degrees per atmosphere)
        let boiling_point = 100.0 + (pressure.0 - 1.0) * 3.0;

        // Convert water currently in the cell into steam
        // The higher the temperature, the more water vaporizes
        if temperature.0 > boiling_point && coolant_level.0 > 0.0 {
            let heat_excess = temperature.0 - boiling_point;
            let available_energy = heat_excess * config.energy_per_heat_unit; // total "extra" thermal energy

            let max_steam_from_energy = available_energy / config.energy_required_per_unit;
            let coolant_boiled = max_steam_from_energy.min(coolant_level.0);

            coolant_level.0 -= coolant_boiled;
            steam_level.0 += coolant_boiled;
        }

        let available_space = (1.0 - coolant_level.0).max(0.01); // prevent div by zero
        let gas_amount = steam_level.0 * config.steam_expansion_ratio;
        let temp_kelvin = (temperature.0 + 273.15).max(0.0);

        let raw_pressure = (gas_amount * temp_kelvin) / available_space;
        let curved_pressure = raw_pressure.powf(config.pressure_curve_exponent);
        pressure.0 = config.nominal_pressure + curved_pressure * config.pressure_scale;

        let potential_steam_output = steam_pull_capacity
            .0
            .min(config.steam_pull_factor * (pressure.0 / config.nominal_pressure));

        let volume_excess = (coolant_level.0 + steam_level.0 - 1.0).max(0.0);
        steam_output.0 = steam_level
            .0
            .min(volume_excess)
            .min(potential_steam_output)
            .max(0.0);
        steam_level.0 -= steam_output.0;

        let space_remaining = 1.0 - (coolant_level.0 + steam_level.0);
        let added_coolant = coolant_flow.0.min(space_remaining);
        coolant_level.0 += added_coolant;
    }
}

#[test]
fn test_generates_steam() {
    let mut app = App::new();
    app.init_resource::<SimulationConfig>();

    app.add_systems(Update, generate_steam);

    let entity = app
        .world_mut()
        .spawn((
            ReactorCell(Position::new(0, 0)),
            SteamOutput::default(),
            CoolantLevel::default(),
            SteamLevel::default(),
            Temperature::default(),
            CoolantFlow::default(),
            Pressure::default(),
            SteamPullCapacity::default(),
        ))
        .id();

    app.update();

    let steam_output = app.world().get::<SteamOutput>(entity).unwrap();

    assert!(steam_output.0 > 0.0, "Steam should be generated");
}
