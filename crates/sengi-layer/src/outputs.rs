//! Deciding which outputs get an instance of the component.

use crate::config::Outputs;

/// The changes that bring the instances in line with the outputs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plan<T> {
    /// Outputs whose instance has to go.
    pub remove: Vec<T>,
    /// Outputs that need an instance built.
    pub add: Vec<T>,
}

/// Works out which instances to drop and which to build.
///
/// `ready` lists the outputs the compositor has finished describing, each
/// with its name if it has one; an output that is announced but not yet
/// described is left out, so its instance is built once its name is known.
/// `instances` lists the outputs that have an instance now.
pub(crate) fn plan<T: Clone + PartialEq>(
    outputs: &Outputs,
    ready: &[(T, Option<String>)],
    instances: &[T],
) -> Plan<T> {
    let wanted: Vec<T> = ready
        .iter()
        .filter(|_| match outputs {
            Outputs::Primary => false,
            Outputs::All => true,
        })
        .map(|(output, _)| output.clone())
        .collect();
    Plan {
        remove: instances
            .iter()
            .filter(|output| !wanted.contains(output))
            .cloned()
            .collect(),
        add: wanted
            .into_iter()
            .filter(|output| !instances.contains(output))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(outputs: &[(u32, &str)]) -> Vec<(u32, Option<String>)> {
        outputs
            .iter()
            .map(|(id, name)| (*id, Some(name.to_string())))
            .collect()
    }

    #[test]
    fn every_output_gets_an_instance() {
        let ready = named(&[(1, "eDP-1"), (2, "HDMI-A-1")]);
        let plan = plan(&Outputs::All, &ready, &[]);
        assert_eq!(plan.add, [1, 2]);
        assert_eq!(plan.remove, []);
    }

    #[test]
    fn nothing_changes_when_instances_match_outputs() {
        let ready = named(&[(1, "eDP-1"), (2, "HDMI-A-1")]);
        let plan = plan(&Outputs::All, &ready, &[2, 1]);
        assert_eq!(plan.add, []);
        assert_eq!(plan.remove, []);
    }

    #[test]
    fn an_unplugged_output_loses_its_instance() {
        let ready = named(&[(1, "eDP-1")]);
        let plan = plan(&Outputs::All, &ready, &[1, 2]);
        assert_eq!(plan.add, []);
        assert_eq!(plan.remove, [2]);
    }

    #[test]
    fn a_plugged_in_output_gains_one_and_the_rest_stay() {
        let ready = named(&[(1, "eDP-1"), (3, "DP-2")]);
        let plan = plan(&Outputs::All, &ready, &[1]);
        assert_eq!(plan.add, [3]);
        assert_eq!(plan.remove, []);
    }

    #[test]
    fn outputs_without_a_name_still_count() {
        let ready = vec![(1, None)];
        assert_eq!(plan(&Outputs::All, &ready, &[]).add, [1]);
    }

    #[test]
    fn the_compositors_choice_is_never_tied_to_an_output() {
        let ready = named(&[(1, "eDP-1"), (2, "HDMI-A-1")]);
        let plan = plan(&Outputs::Primary, &ready, &[]);
        assert_eq!(plan.add, []);
        assert_eq!(plan.remove, []);
    }
}
