# Query only the selected existing server, never the command being completed.
function __fish_tmuxx_targets
    set -l kind $argv[1]
    set -l tokens (commandline -xpc)
    argparse -s (__fish_tmuxx_global_optspecs) -- $tokens[2..] 2>/dev/null
    or return

    set -l server_args
    set -q _flag_L; and set -a server_args -L "$_flag_L"
    set -q _flag_S; and set -a server_args -S "$_flag_S"
    set -q _flag_native_endpoint; and set -a server_args --native-endpoint "$_flag_native_endpoint"
    set -q _flag_imsg_schema; and set -a server_args --imsg-schema "$_flag_imsg_schema"

    set -l query
    switch $kind
        case session
            set query list-sessions -F '#{session_name}'\t'#{session_windows} windows'\n'#{session_id}'\t'#{session_name} (#{session_windows} windows)'
        case window
            set query list-windows -a -F '#{session_name}:#{window_index}'\t'#{window_name}'\n'#{window_id}'\t'#{session_name}:#{window_index} #{window_name}'
        case pane
            set query list-panes -a -F '#{pane_id}'\t'#{session_name}:#{window_index}.#{pane_index} #{pane_current_command}'
        case client
            set query list-clients -F '#{client_name}'\t'#{session_name}'
        case buffer
            set query list-buffers -F '#{buffer_name}'\t'#{buffer_size} bytes'
        case '*'
            return
    end
    # shortcut: queries await server replies; add a timeout if stuck servers stall completion.
    command $tokens[1] $server_args -N $query 2>/dev/null
end
