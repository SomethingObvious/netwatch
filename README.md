# Netwatch

A small desktop board for Windows that tells you whether your internet has been dropping packets badly enough in the last hour to be worth restarting the router.

![The board when the line is fine, worth watching, and due for a reset](board.png)

It pings 1.1.1.1 and 8.8.8.8 once a second, and your router as well. A minute counts as bad when both internet hosts lost at least two pings in it, and the board says RESET once there have been three bad minutes in the hour with one of them in the last 10. Pinging the router too is what tells the line apart from your own network, since a router that answers every ping while the internet doesn't means the loss is past it.

The top row is the verdict. CLEAN means nothing was lost, FINE means a ping here and there, WATCH means there has been a bad minute, and RECOVERY means it has been quiet for 10 minutes after a bad spell. Under it are the loss over the last 5 minutes, the ping to the internet and to the router in ms, and the number of bad minutes this hour. The strip at the bottom is the hour, one cell per minute, with the newest on the right.

It sits on the desktop under your windows, so it never covers a game, and it uses about 2 MB of memory and 150 bytes a second of traffic. Drag it wherever you like and right-click it for Start With Windows or Close. The hour is kept on disk, so a reboot doesn't lose it.

## Building

It needs Rust and nothing else, as it calls Win32 directly.

```
cargo build --release
```

That gives you `target\release\netwatch.exe`. Its position and the last hour are kept in `%LOCALAPPDATA%\Netwatch`.

It won't fix the connection. It only tells you when a restart is worth a try, and it is Windows only.
