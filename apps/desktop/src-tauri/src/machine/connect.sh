# Boat runs these commands as the provisioned T3 user on the Agent37 VM.
set -eu
umask 077
state="$HOME/.t3agent37"
mkdir -p "$state"
setup="$state/connect-setup"
t3="$HOME/.local/bin/t3"

# Serialize the remote requests even if Boat drops an in-flight exec or restarts.
exec 9>"$state/connect.lock"
flock -x 9

connect_child_running() {
    case "$(tr '\0' ' ' <"/proc/$1/cmdline" 2>/dev/null || true)" in
        *" connect link --headless "*) return 0 ;;
        *) return 1 ;;
    esac
}

clear_connect_setup() {
    if [ ! -f "$setup/exit" ] && [ -s "$setup/pid" ] && read -r child <"$setup/pid"; then
        case "$child" in
            ''|*[!0-9]*) return 1 ;;
        esac
        if [ "$child" -gt 1 ] && connect_child_running "$child"; then
            kill -TERM "$child" 2>/dev/null || true
            for attempt in 1 2 3 4 5 6 7 8 9 10; do
                [ -f "$setup/exit" ] && break
                sleep 0.2
            done
            if connect_child_running "$child"; then
                echo "T3 Connect enrollment has not stopped." >&2
                return 1
            fi
        fi
    fi
    rm -rf "$setup"
}

start_connect_setup() {
    [ ! -e "$state/connect-deleting" ] || return 1
    clear_connect_setup
    mkdir "$setup"
    (
        "$t3" connect link --headless &
        child=$!
        printf '%s\n' "$child" >"$setup/pid"
        status=0
        wait "$child" || status=$?
        printf '%s\n' "$status" >"$setup/exit"
    ) </dev/null >"$setup/log" 2>&1 9>&- &
    # Publish the child PID before an unlink can acquire the lock.
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        [ -s "$setup/pid" ] && return 0
        sleep 0.1
    done
    return 1
}

poll_connect_setup() {
    test -d "$setup"
    if [ -f "$setup/exit" ]; then cat "$setup/exit"; fi
    echo ---
    cat "$setup/log"
}

unlink_connect() {
    # A delayed setup request must never enroll the machine after cleanup.
    touch "$state/connect-deleting"
    clear_connect_setup
    timeout --kill-after=5s 35s "$t3" connect unlink --log-level none
}
