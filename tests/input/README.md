# Input regression tests

These scripts compile production input handlers against deterministic stubs
and recording platform backends. They do not inject input into the test
machine. ctest runs them as `regression_delayed_mouse_release`,
`regression_windows_absolute_mouse` and `regression_windows_mouse_keys`; each
also runs directly from the repository root:

```sh
python3 tests/input/test_delayed_mouse_release.py
python3 tests/input/test_windows_absolute_mouse.py
python3 tests/input/test_windows_mouse_keys.py
```

The scripts do not replace a full platform build or driver-level testing.

## Windows integration check

Use Moonlight, including two connected clients:

1. Hold an ordinary key, then each of Alt/Ctrl/Shift/Win, and abruptly disconnect
   the client. Check that host `GetAsyncKeyState` reports the streamed keys up
   and that typing and Explorer double-click work normally.
2. Repeat with a keybinding remap, including an ordinary key mapped to another
   ordinary key and Alt mapped to Win. Change the mapping while the key is held;
   the originally pressed host key must be released.
3. Hold each mouse button, including X1 and X2, while disconnecting. Test a drag
   using absolute input and a disconnect immediately after releasing left-click.
4. Hold controller buttons, Back (before its Home timer expires), triggers and
   both sticks off-center. Disconnect the stream, and separately unplug one
   controller while another keeps sending packets. Verify neutral state/device
   removal and no delayed Home press.
5. Disconnect during a touch drag, pen contact/hover with barrel buttons, and a
   controller touchpad contact. Verify that contacts and devices are removed.
6. Hold the same key from two clients. Disconnect one; the other must retain its
   key until release. Check that another client's key-up cannot release it.
7. Repeat disconnect while capture teardown is delayed. Input cleanup should
   run before the capture joins finish. Verify healthy Alt+Tab, typing, mouse,
   controller and pen/touch use after reconnect.
