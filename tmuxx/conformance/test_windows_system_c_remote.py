"""Restricted oracle entry validation; no desktop or remote-C pass implied."""
import unittest
import windows_system_c_remote as entry


class FixedSystemEntryTests(unittest.TestCase):
    def test_only_exact_control_and_buffer_observation_forms_reach_selection(self):
        for command in ("exec tmux -C attach-session -f new-layouts",
                        "exec tmux -C attach-session -f new-layouts -t '$12'",
                        "exec tmux list-buffers -F '#{buffer_name}'"):
            with self.subTest(command=command), self.assertRaisesRegex(ValueError, "missing selected"):
                entry.arguments({}, command)
        for command in ("tmux", "exec tmux new-session", "exec tmux -C attach-session -f new-layouts; id",
                        "exec tmux -C attach-session -f new-layouts -t '$1;id'",
                        "exec tmux -C attach-session -f new-layouts\n", "exec tmux list-buffers -F '#{buffer_size}'"):
            with self.subTest(command=command), self.assertRaisesRegex(ValueError, "unsupported fixed"):
                entry.arguments({}, command)

    def test_invalid_expired_and_unbounded_leases_fail_before_process_access(self):
        for expiry in (0, -1, 2 ** 60):
            with self.subTest(expiry=expiry), self.assertRaisesRegex(ValueError, "lease expired or invalid"):
                entry.arguments({"schema": 1, "server_pid": 12, "expires_unix": expiry},
                                "exec tmux -C attach-session -f new-layouts")


if __name__ == "__main__":
    unittest.main()
