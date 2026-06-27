use super::*;

impl<'a> Interpreter<'a> {
    // === Allocator ===
    pub(crate) fn builtin_allocator_system(&self, _args: Vec<Value>) -> Result<Value, InterpError> {
        Ok(Value::Allocator(AllocatorKind::System))
    }

    pub(crate) fn builtin_allocator_arena(&self, _args: Vec<Value>) -> Result<Value, InterpError> {
        Ok(Value::Allocator(AllocatorKind::Arena))
    }

    pub(crate) fn builtin_allocator_bump(&self, _args: Vec<Value>) -> Result<Value, InterpError> {
        Ok(Value::Allocator(AllocatorKind::Bump))
    }

    pub(crate) fn builtin_alloc(&mut self, args: Vec<Value>) -> Result<Value, InterpError> {
        // alloc(allocator, value) - allocate a value with the given allocator
        if args.len() != 2 {
            return Err(InterpError::new(
                "alloc expects 2 arguments (allocator, value)",
            ));
        }
        let alloc_val = &args[0];
        let value = &args[1];
        match alloc_val {
            Value::Allocator(kind) => match kind {
                AllocatorKind::System => {
                    // System allocator: just return the value as-is (heap allocated)
                    Ok(value.clone())
                }
                AllocatorKind::Arena => {
                    // Arena allocator: allocate in current arena if available
                    if self.arenas.is_empty() {
                        return Err(InterpError::new(
                            "alloc: no arena available (use arena block)",
                        ));
                    }
                    let arena_id = self.arenas.len() - 1;
                    let idx = self.arenas[arena_id].slots.len();
                    self.arenas[arena_id].slots.push(value.clone());
                    let gen = self.arenas[arena_id].generation;
                    Ok(Value::ArenaRef(arena_id, idx, gen))
                }
                AllocatorKind::Bump => {
                    // Bump allocator: same as arena (monotonic allocation)
                    if self.arenas.is_empty() {
                        return Err(InterpError::new(
                            "alloc: no arena available (use alloc(Bump) block)",
                        ));
                    }
                    let arena_id = self.arenas.len() - 1;
                    let idx = self.arenas[arena_id].slots.len();
                    self.arenas[arena_id].slots.push(value.clone());
                    let gen = self.arenas[arena_id].generation;
                    Ok(Value::ArenaRef(arena_id, idx, gen))
                }
            },
            _ => Err(InterpError::new(
                "alloc first argument must be an Allocator value",
            )),
        }
    }

    pub(crate) fn builtin_arena_reset(&mut self, _args: Vec<Value>) -> Result<Value, InterpError> {
        // arena_reset() - reset all arena allocations and invalidate stale ArenaRefs
        if !self.arenas.is_empty() {
            let arena_id = self.arenas.len() - 1;
            self.arenas[arena_id].slots.clear();
            self.arenas[arena_id].generation += 1;
        }
        Ok(Value::Unit)
    }

    pub(crate) fn builtin_bump_used(&self, _args: Vec<Value>) -> Result<Value, InterpError> {
        // bump_used() - return the number of bump allocations
        if self.arenas.is_empty() {
            return Ok(Value::Int(0));
        }
        let arena_id = self.arenas.len() - 1;
        Ok(Value::Int(self.arenas[arena_id].slots.len() as i64))
    }
}
