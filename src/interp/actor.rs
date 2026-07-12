use super::*;

impl<'a> Interpreter<'a> {
    pub(crate) fn spawn_actor(&mut self, actor_name: &str) -> Result<Value, InterpError> {
        // v0.29.24: spawn quota — process-wide max_children from @max_children(N).
        // v0.29.31: per-actor-type spawn quota from @max_children on the flow.
        if let Some(flow) = self.flow_index.get(actor_name) {
            for ann in &flow.annotations {
                if let crate::ast::FlowAnnotation::MaxChildren(n) = ann {
                    let count = self.actor_spawn_counts
                        .get(actor_name)
                        .copied()
                        .unwrap_or(0);
                    if count >= *n {
                        return Err(InterpError::new(
                            &format!(
                                "QuotaExceeded: spawn would exceed @max_children({}) for actor '{}' (current {})",
                                n, actor_name, count
                            ),
                        ));
                    }
                }
            }
        }

        // v0.29.24: spawn quota — process-wide max_children from @max_children(N).
        if let Some(max) = self.max_children {
            if self.spawn_count >= max {
                return Err(InterpError::new(
                    "QuotaExceeded: spawn would exceed @max_children limit",
                ));
            }
        }
        let actor_def = self
            .find_actor(actor_name)
            .ok_or_else(|| format!("actor '{}' not found", actor_name))?;

        // Create actor instance with initialized fields
        let mut fields = HashMap::new();
        for field in &actor_def.fields {
            let value = field
                .init
                .as_ref()
                .map(|e| self.eval_expr(e))
                .transpose()?
                .unwrap_or_else(|| match &field.ty {
                    Type::Name(n, _) if n == "i32" => Value::Int(0),
                    Type::Name(n, _) if n == "f64" => Value::Float(0.0),
                    Type::Name(n, _) if n == "bool" => Value::Bool(false),
                    Type::Name(n, _) if n == "string" => Value::String(String::new()),
                    _ => Value::Unit,
                });
            fields.insert(field.name.clone(), value);
        }

        let instance = ActorInstance {
            actor_name: actor_name.to_string(),
            fields,
            methods: actor_def.methods.clone(),
            faulted: false,
            peer_links: Vec::new(),
            parent_id: crate::interp::value::CURRENT_ACTOR_ID.with(|id| {
                let id = id.get();
                if id == 0 { None } else { Some(id) }
            }),
            is_detached: false,
            producers: Vec::new(),
        };

        // v0.28.28 fix for #1: share the spawning program's AST so the
        // worker thread's Interpreter can resolve user-defined functions
        // and types when executing actor methods.
        let program = std::sync::Arc::new(self.file.clone());
        let handle = ActorHandle::new(instance, program);
        self.spawn_count += 1;
        // per-actor-type tracking
        *self.actor_spawn_counts.entry(actor_name.to_string()).or_insert(0) += 1;
        // v0.29.31: auto-apply @mailbox(depth=N) from flow annotations.
        if let Some(flow) = self.flow_index.get(actor_name) {
            for ann in &flow.annotations {
                if let crate::ast::FlowAnnotation::MailboxDepth(d) = ann {
                    handle.set_mailbox_depth_limit(*d);
                    break;
                }
            }
        }
        Ok(Value::Actor(handle))
    }

    /// v0.29.24: remaining spawn quota (`None` if unlimited).
    pub(crate) fn spawn_quota_remaining(&self) -> Option<usize> {
        self.max_children.map(|m| m.saturating_sub(self.spawn_count))
    }

    /// v0.29.24: set process-wide max children (for tests / runtime reconfigure).
    pub(crate) fn set_max_children(&mut self, n: Option<usize>) {
        self.max_children = n;
    }

    /// v0.29.37: spawn a detached actor — survives parent SystemKill.
    pub(crate) fn spawn_detached_actor(&mut self, actor_name: &str) -> Result<Value, InterpError> {
        let handle_val = self.spawn_actor(actor_name)?;
        if let Value::Actor(ref handle) = handle_val {
            // Mark as detached
            if let Ok(mut instance) = handle.inner.write() {
                instance.is_detached = true;
                instance.parent_id = None;
            }
        }
        Ok(handle_val)
    }

    /// v0.29.37: SystemKill — cascade terminate all non-detached children
    /// of the given parent actor id. Called when parent faults or is dropped.
    /// H5-fix: We use unwrap_or_else(|e| e.into_inner()) to recover from
    /// mutex poison. This is intentional: during SystemKill, an actor thread
    /// may have panicked while holding the lock. Aborting the entire cascade
    /// would be worse than proceeding with potentially stale data, since
    /// we're killing children anyway. The lock is only used for the actor
    /// registry, not for mutable actor state.
    pub(crate) fn system_kill_children(&self, parent_id: usize) {
        let handles = crate::interp::value::actor_handles();
        // SAFETY: into_inner() on a poisoned mutex is safe — it returns the
        // inner data. The data may be inconsistent, but we only read the
        // parent_id and is_detached fields which are set at spawn time and
        // not modified during normal operation.
        let registry = handles.lock().unwrap_or_else(|e| e.into_inner());
        let child_ids: Vec<usize> = registry
            .iter()
            .filter(|(_, h)| {
                if let Ok(instance) = h.inner.read() {
                    instance.parent_id == Some(parent_id) && !instance.is_detached
                } else {
                    false
                }
            })
            .map(|(id, _)| *id)
            .collect();
        drop(registry);
        // Kill each child
        for child_id in child_ids {
            let handles = crate::interp::value::actor_handles();
            let registry = handles.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(child) = registry.get(&child_id) {
                // Mark as faulted (short-circuit mailbox)
                if let Ok(mut instance) = child.inner.write() {
                    instance.faulted = true;
                }
                // Recursively kill grandchildren
                drop(registry);
                self.system_kill_children(child_id);
            }
        }
    }
}
