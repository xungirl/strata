# Shared helpers for benchmark scripts (source this file).

# Block until the 1-minute load average is below $MAX_LOAD (default 4), polling
# every 30 s for at most $MAX_WAIT_S (default 1800). Appends what happened to
# $LOAD_LOG. Another workload on this machine (e.g. a kind cluster in colima)
# can otherwise distort timings.
wait_for_quiet() {
  local label=$1 max=${MAX_LOAD:-4} waited=0 limit=${MAX_WAIT_S:-1800}
  while true; do
    local l1
    l1=$(sysctl -n vm.loadavg | awk '{print $2}')
    if awk -v l="$l1" -v m="$max" 'BEGIN{exit !(l < m)}'; then
      echo "$(date -u +%FT%TZ) $label load1=$l1 waited=${waited}s ok" >> "$LOAD_LOG"
      return 0
    fi
    if [ "$waited" -ge "$limit" ]; then
      echo "$(date -u +%FT%TZ) $label load1=$l1 waited=${waited}s PROCEEDING-UNDER-LOAD" >> "$LOAD_LOG"
      return 0
    fi
    sleep 30
    waited=$((waited + 30))
  done
}
