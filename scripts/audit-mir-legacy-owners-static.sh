#!/usr/bin/env bash
# Run the static owner inventory without dynamic probes. This entry point is
# intentionally distinct from the complete audit and never reports audit_status.
set -euo pipefail

if [ "${MIMI_LEGACY_OWNER_AUDIT_SKIP_DYNAMIC_PROBES:-0}" != "0" ]; then
    printf 'static_audit_error=dynamic_probe_skip_flag_is_not_supported value=%s\n' \
        "$MIMI_LEGACY_OWNER_AUDIT_SKIP_DYNAMIC_PROBES" >&2
    exit 2
fi

ROOT_DIR="$(cd -P "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec bash "$ROOT_DIR/scripts/audit-mir-legacy-owners.sh" --static-only
