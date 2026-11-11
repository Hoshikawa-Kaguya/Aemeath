# Usage guide

[中文](usage.md) | English

"Input" and "output" are always named from Aemeath's side: sound entering Aemeath comes from an input, sound leaving it goes to an output. Full definitions are in [`GLOSSARY.md`](../GLOSSARY.md) (Chinese).

## 1. First start

1. Run `Aemeath-win-Setup.exe` from [Releases](https://github.com/Hoshikawa-Kaguya/Aemeath/releases), or unzip the portable build and run `Aemeath.exe`.
2. If the usbip-win2 driver is missing, Aemeath explains why it is needed and offers to install it. This needs administrator rights once and takes about a minute; a restart may be required.
3. Aemeath then registers its 16 virtual devices. The first time, Windows installs a driver for each one, which can take about a minute.
4. The workbench starts empty. Click "Use the workbench preset" for one virtual input and one virtual output, then add what you need.

Closing the window keeps Aemeath running in the notification area. Use "Quit Aemeath" in the tray menu to exit. Starting Aemeath again from the Start menu or taskbar opens the running window.

## 2. Workbench

Each box is a **node**, and each line between nodes is a **route**. Sound flows from input nodes through optional **buses** (which sum several sources) to output nodes.

| Node | Purpose |
|---|---|
| External input | A microphone or other recording device |
| External output | Headphones, speakers or another playback device |
| Virtual input | A virtual playback device that apps can play into |
| Virtual output | A virtual recording device that OBS and other apps can record from |
| App source | A copy of one program's sound, wherever it plays |
| Bus | Sums what is sent to it and passes it on |
| External insert | Sends sound through an external program (e.g. NVIDIA Broadcast) and takes it back |
| Recorder | Writes what is sent to it into a file |

- **Add node** from the toolbar, or right-click empty space.
- **Connect** by dragging from a node's port onto another node. Nodes that cannot be connected explain why while you drag.
- **Gain**: usually adjusted on the nodes. To adjust a single route, right-click it and choose "Show route label", then hover the label and scroll (1 dB per step); click the dot on the label to turn the route on or off. Route labels are hidden by default.
- **Delete** with Del, undo with Ctrl+Z. **Node settings…** in the node menu changes the name, slot, gate, device format and resampler.

The **route matrix** shows every route as a source × destination grid. The **console** shows pinned nodes as channel strips ("Pin node", or "Pin to console" in a node's menu).

## 3. Virtual devices and per-app routing

In Windows, virtual inputs appear as playback devices such as "Speakers (Aemeath In 1)", and virtual outputs as recording devices such as "Line (Aemeath Out 1)". Only devices used by a node are plugged in; the rest are hidden. The **Devices** page shows all 16 slots.

To route an app, either pick its **landing** on the **Apps** page (saved per program; some programs such as Discord choose their own device and must be set inside the program), or add an **app source** node, which copies the app's sound without changing where it plays.

## 4. External devices

External devices are opened in exclusive mode by default for lower latency; switch a node to shared mode in its settings if another program needs the device too. Unplugged or busy devices show as offline and reconnect automatically. Use "Re-detect device" or "Re-detect all devices" to retry right away.

## 5. Recording and hotkeys

Add a recorder node and connect what you want to record; start and stop it on the **Recordings** page or in the console. Set hotkeys on the **Hotkeys** page: global hotkeys (all recorders, panic mute, show/hide window) and hotkeys owned by a node or route. Each works as **Toggle** or **Hold**.

## 6. Uninstalling

- Installer build: uninstall from Windows "Installed apps". Aemeath asks for administrator rights once to remove its virtual devices.
- Portable build: run "Remove virtual devices" on the Settings page first, then delete the folder.

The usbip-win2 driver is not removed, since other programs may use it; uninstall it separately if you no longer need it.

## 7. Reporting problems

Open an [issue](https://github.com/Hoshikawa-Kaguya/Aemeath/issues) and attach the logs from "Open log folder" on the Settings page (`%APPDATA%\Aemeath\logs`). Enabling "Crash dumps" on the Settings page makes crashes easier to diagnose.
