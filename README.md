# plexplayerd

plexplayerd plays your Plex music through the speakers attached to the computer it
runs on. Start it on that machine and a new player shows up in the Plex app. Pick it,
press play, and the music comes out of those speakers.

## What you need

- A Plex Media Server on your home network, with music in its library.
- A Linux machine connected to speakers: built in, USB, or a receiver.
- The Plex app on your phone, tablet, or computer, on the same network.

## Getting it running

From the terminal on the machine with the speakers:

```
./plexplayerd --name "Kitchen"
```

With Nix:

```
nix run .#plexplayerd -- --name "Kitchen"
```

The name is the label you see in the Plex app. If you leave it out, the machine's
hostname is used. Run `./plexplayerd --help` to see the options.

The program prints a line saying it is listening, then keeps running.

## Playing music

1. Start plexplayerd on the machine with the speakers.
2. Open the Plex app and pick an album or playlist.
3. Choose your player by its name as the device to play on.
4. Control everything from the app: play, pause, skip, next album, volume, queue.

It moves on to the next track by itself and keeps playing until you stop it in the app.

## Choosing the speakers

Sound goes to the machine's default speakers. If nothing comes out, or the wrong
device is used, name the one you want:

```
PLEX_ALSA_DEVICE=plughw:0,0 ./plexplayerd --name "Kitchen"
```

## If something goes wrong

- **The player is not in the app's list.** Check that plexplayerd is running and that
  the phone and the machine are on the same network, then reopen the app.
- **No sound.** Check the machine's own volume, and that you named the right speakers.
- **Music stops after one track.** Keep the Plex app open on the now playing screen.
  The player moves to the next track when the app checks in.
- **Volume is low.** The app's volume controls it. Set the app near full and adjust
  the level on the speakers.
- **It cannot reach your library.** Point it at the right Plex Media Server:

  ```
  PLEX_SERVER=http://192.168.1.20:32400 ./plexplayerd --name "Kitchen"
  ```

## Stopping it

Press Ctrl+C in the terminal where it runs. It does not keep running in the
background, and it does not start again by itself after a reboot.

## Good to know

- Music only. It does not play video.
- Anyone on your network can start and stop playback, so use it on a home network.
- Run one copy per room you want sound in.
- No account, no config file, and nothing is stored on disk.
