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
}
