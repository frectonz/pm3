use crate::config::ProcessConfig;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum DepsError {
    #[error("circular dependency detected: {}", cycle.join(" -> "))]
    Circular { cycle: Vec<String> },
    #[error("process '{from}' depends on unknown process '{to}'")]
    Missing { from: String, to: String },
}

/// Check that every name in `depends_on` lists actually exists as a config key.
pub fn validate_deps(configs: &HashMap<String, ProcessConfig>) -> Result<(), DepsError> {
    for (name, config) in configs {
        if let Some(deps) = &config.depends_on {
            for dep in deps {
                if !configs.contains_key(dep) {
                    return Err(DepsError::Missing {
                        from: name.clone(),
                        to: dep.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Returns processes grouped by level: level 0 has no dependencies,
/// level 1 depends only on level 0, etc. Detects circular dependencies.
pub fn topological_levels(
    configs: &HashMap<String, ProcessConfig>,
) -> Result<Vec<Vec<String>>, DepsError> {
    // Build in-degree map and adjacency list (dep -> vec of dependents)
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

    for name in configs.keys() {
        in_degree.entry(name.as_str()).or_insert(0);
        dependents.entry(name.as_str()).or_default();
    }

    for (name, config) in configs {
        if let Some(deps) = &config.depends_on {
            *in_degree.entry(name.as_str()).or_insert(0) += deps.len();
            for dep in deps {
                dependents
                    .entry(dep.as_str())
                    .or_default()
                    .push(name.as_str());
            }
        }
    }

    let mut levels: Vec<Vec<String>> = Vec::new();
    let mut queue: VecDeque<&str> = VecDeque::new();

    // Seed with nodes that have in_degree 0
    for (&name, &deg) in &in_degree {
        if deg == 0 {
            queue.push_back(name);
        }
    }

    let mut processed = 0usize;

    while !queue.is_empty() {
        let mut level: Vec<String> = Vec::new();
        let level_size = queue.len();

        for _ in 0..level_size {
            let node = queue.pop_front().unwrap();
            level.push(node.to_string());
            processed += 1;

            for &dependent in &dependents[node] {
                let deg = in_degree.get_mut(dependent).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(dependent);
                }
            }
        }

        level.sort(); // deterministic ordering within a level
        levels.push(level);
    }

    if processed != configs.len() {
        // Find cycle via DFS
        let cycle = find_cycle(configs);
        return Err(DepsError::Circular { cycle });
    }

    Ok(levels)
}

/// DFS-based cycle finder for error reporting.
fn find_cycle(configs: &HashMap<String, ProcessConfig>) -> Vec<String> {
    let mut visited = HashSet::new();
    let mut on_stack = HashSet::new();
    let mut parent: HashMap<&str, &str> = HashMap::new();

    let mut names: Vec<&str> = configs.keys().map(|s| s.as_str()).collect();
    names.sort(); // deterministic start order

    for &start in &names {
        if visited.contains(start) {
            continue;
        }
        let mut stack: Vec<(&str, bool)> = vec![(start, false)];
        while let Some((node, returning)) = stack.pop() {
            if returning {
                on_stack.remove(node);
                continue;
            }
            if on_stack.contains(node) {
                // Reconstruct cycle
                let mut cycle = vec![node.to_string()];
                let mut cur = parent.get(node).copied();
                while let Some(p) = cur {
                    cycle.push(p.to_string());
                    if p == node {
                        break;
                    }
                    cur = parent.get(p).copied();
                }
                cycle.reverse();
                return cycle;
            }
            if visited.contains(node) {
                continue;
            }
            visited.insert(node);
            on_stack.insert(node);
            stack.push((node, true)); // return marker

            if let Some(config) = configs.get(node)
                && let Some(deps) = &config.depends_on
            {
                for dep in deps {
                    parent.insert(dep.as_str(), node);
                    stack.push((dep.as_str(), false));
                }
            }
        }
    }

    vec!["unknown cycle".to_string()]
}

/// Flat reverse of topological levels: dependents come before their dependencies.
pub fn reverse_stop_order(
    configs: &HashMap<String, ProcessConfig>,
) -> Result<Vec<String>, DepsError> {
    let levels = topological_levels(configs)?;
    let mut order: Vec<String> = levels.into_iter().flatten().collect();
    order.reverse();
    Ok(order)
}

/// Given a set of requested names, expand to include all transitive dependencies.
/// Returns names in topological order (dependencies first).
pub fn expand_deps(
    names: &[String],
    configs: &HashMap<String, ProcessConfig>,
) -> Result<Vec<String>, DepsError> {
    let mut needed: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    for name in names {
        if !needed.contains(name) {
            needed.insert(name.clone());
            queue.push_back(name.clone());
        }
    }

    while let Some(name) = queue.pop_front() {
        if let Some(config) = configs.get(&name)
            && let Some(deps) = &config.depends_on
        {
            for dep in deps {
                if !needed.contains(dep) {
                    needed.insert(dep.clone());
                    queue.push_back(dep.clone());
                }
            }
        }
    }

    // Build subset configs for ordering
    let subset: HashMap<String, ProcessConfig> = configs
        .iter()
        .filter(|(k, _)| needed.contains(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let levels = topological_levels(&subset)?;
    Ok(levels.into_iter().flatten().collect())
}

/// Given target names, expand to include all transitive dependents (processes
/// that depend on the targets). Returns in reverse topological order
/// (dependents first, so they can be stopped before their dependencies).
pub fn expand_dependents(
    targets: &[String],
    configs: &HashMap<String, ProcessConfig>,
) -> Result<Vec<String>, DepsError> {
    // Build reverse adjacency: dep -> set of dependents
    let mut reverse_adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for name in configs.keys() {
        reverse_adj.entry(name.as_str()).or_default();
    }
    for (name, config) in configs {
        if let Some(deps) = &config.depends_on {
            for dep in deps {
                reverse_adj
                    .entry(dep.as_str())
                    .or_default()
                    .push(name.as_str());
            }
        }
    }

    // BFS from targets through reverse adjacency
    let mut needed: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    for t in targets {
        if !needed.contains(t) {
            needed.insert(t.clone());
            queue.push_back(t.clone());
        }
    }

    while let Some(name) = queue.pop_front() {
        if let Some(deps_of) = reverse_adj.get(name.as_str()) {
            for &dependent in deps_of {
                if !needed.contains(dependent) {
                    needed.insert(dependent.to_string());
                    queue.push_back(dependent.to_string());
                }
            }
        }
    }

    // Build subset and return in reverse topo order
    let subset: HashMap<String, ProcessConfig> = configs
        .iter()
        .filter(|(k, _)| needed.contains(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let levels = topological_levels(&subset)?;
    let mut order: Vec<String> = levels.into_iter().flatten().collect();
    order.reverse();
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(deps: Option<Vec<&str>>) -> ProcessConfig {
        ProcessConfig {
            command: "echo hi".to_string(),
            cwd: None,
            env: None,
            env_file: None,
            health_check: None,
            kill_timeout: None,
            kill_signal: None,
            max_restarts: None,
            max_memory: None,
            min_uptime: None,
            stop_exit_codes: None,
            watch: None,
            watch_ignore: None,
            watch_debounce: None,
            depends_on: deps.map(|v| v.into_iter().map(|s| s.to_string()).collect()),
            restart: None,
            group: None,
            pre_start: None,
            post_stop: None,
            cron_restart: None,
            log_date_format: None,
            instances: None,
            environments: HashMap::new(),
        }
    }

    #[test]
    fn test_validate_missing_dep() {
        let mut configs = HashMap::new();
        configs.insert("web".to_string(), cfg(Some(vec!["db"])));
        let err = validate_deps(&configs).unwrap_err();
        assert_eq!(
            err,
            DepsError::Missing {
                from: "web".to_string(),
                to: "db".to_string(),
            }
        );
    }

    #[test]
    fn test_validate_all_present() {
        let mut configs = HashMap::new();
        configs.insert("db".to_string(), cfg(None));
        configs.insert("web".to_string(), cfg(Some(vec!["db"])));
        assert!(validate_deps(&configs).is_ok());
    }

    #[test]
    fn test_topo_no_deps() {
        let mut configs = HashMap::new();
        configs.insert("a".to_string(), cfg(None));
        configs.insert("b".to_string(), cfg(None));
        let levels = topological_levels(&configs).unwrap();
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0], vec!["a", "b"]);
    }

    #[test]
    fn test_topo_linear_chain() {
        let mut configs = HashMap::new();
        configs.insert("a".to_string(), cfg(None));
        configs.insert("b".to_string(), cfg(Some(vec!["a"])));
        configs.insert("c".to_string(), cfg(Some(vec!["b"])));
        let levels = topological_levels(&configs).unwrap();
        assert_eq!(levels, vec![vec!["a"], vec!["b"], vec!["c"]]);
    }

    #[test]
    fn test_topo_diamond() {
        // a -> b, a -> c, b -> d, c -> d
        let mut configs = HashMap::new();
        configs.insert("a".to_string(), cfg(None));
        configs.insert("b".to_string(), cfg(Some(vec!["a"])));
        configs.insert("c".to_string(), cfg(Some(vec!["a"])));
        configs.insert("d".to_string(), cfg(Some(vec!["b", "c"])));
        let levels = topological_levels(&configs).unwrap();
        assert_eq!(levels[0], vec!["a"]);
        assert_eq!(levels[1], vec!["b", "c"]);
        assert_eq!(levels[2], vec!["d"]);
    }

    #[test]
    fn test_topo_parallel_roots() {
        let mut configs = HashMap::new();
        configs.insert("db".to_string(), cfg(None));
        configs.insert("cache".to_string(), cfg(None));
        configs.insert("web".to_string(), cfg(Some(vec!["db", "cache"])));
        let levels = topological_levels(&configs).unwrap();
        assert_eq!(levels[0], vec!["cache", "db"]);
        assert_eq!(levels[1], vec!["web"]);
    }

    #[test]
    fn test_circular_two_nodes() {
        let mut configs = HashMap::new();
        configs.insert("a".to_string(), cfg(Some(vec!["b"])));
        configs.insert("b".to_string(), cfg(Some(vec!["a"])));
        let err = topological_levels(&configs).unwrap_err();
        assert!(matches!(err, DepsError::Circular { .. }));
    }

    #[test]
    fn test_circular_three_nodes() {
        let mut configs = HashMap::new();
        configs.insert("a".to_string(), cfg(Some(vec!["c"])));
        configs.insert("b".to_string(), cfg(Some(vec!["a"])));
        configs.insert("c".to_string(), cfg(Some(vec!["b"])));
        let err = topological_levels(&configs).unwrap_err();
        assert!(matches!(err, DepsError::Circular { .. }));
    }

    #[test]
    fn test_circular_self_dependency() {
        let mut configs = HashMap::new();
        configs.insert("a".to_string(), cfg(Some(vec!["a"])));
        let err = topological_levels(&configs).unwrap_err();
        assert!(matches!(err, DepsError::Circular { .. }));
    }

    #[test]
    fn test_reverse_order() {
        let mut configs = HashMap::new();
        configs.insert("db".to_string(), cfg(None));
        configs.insert("web".to_string(), cfg(Some(vec!["db"])));
        let order = reverse_stop_order(&configs).unwrap();
        assert_eq!(order, vec!["web", "db"]);
    }

    #[test]
    fn test_expand_deps() {
        let mut configs = HashMap::new();
        configs.insert("db".to_string(), cfg(None));
        configs.insert("cache".to_string(), cfg(None));
        configs.insert("web".to_string(), cfg(Some(vec!["db", "cache"])));
        let expanded = expand_deps(&["web".to_string()], &configs).unwrap();
        // Should include db and cache before web
        assert_eq!(expanded.len(), 3);
        let web_idx = expanded.iter().position(|n| n == "web").unwrap();
        let db_idx = expanded.iter().position(|n| n == "db").unwrap();
        let cache_idx = expanded.iter().position(|n| n == "cache").unwrap();
        assert!(db_idx < web_idx);
        assert!(cache_idx < web_idx);
    }

    #[test]
    fn test_expand_dependents() {
        let mut configs = HashMap::new();
        configs.insert("db".to_string(), cfg(None));
        configs.insert("web".to_string(), cfg(Some(vec!["db"])));
        configs.insert("worker".to_string(), cfg(Some(vec!["db"])));
        let order = expand_dependents(&["db".to_string()], &configs).unwrap();
        // Should include web and worker, and they come before db (reverse topo)
        assert_eq!(order.len(), 3);
        let db_idx = order.iter().position(|n| n == "db").unwrap();
        let web_idx = order.iter().position(|n| n == "web").unwrap();
        let worker_idx = order.iter().position(|n| n == "worker").unwrap();
        assert!(web_idx < db_idx);
        assert!(worker_idx < db_idx);
    }
}
