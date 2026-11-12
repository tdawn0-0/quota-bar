# The panel

<img src="images/panel.png" width="416" alt="The panel with all six providers enabled">

The panel is a 344 pt wide popup that opens under the menu bar item on macOS
(and from the tray menu on Linux). It is a translucent sheet with a hairline
top edge, a fixed header and footer, and a scrolling list of provider cards.
The labels are in Chinese because the numbers are read at a glance.

The screenshot above has all six providers enabled, scrolled to the top; the
list continues below the window and scrolls.

## Anatomy

**Header.** The title, how long ago the data was refreshed (`22 秒前`, or
`未刷新` before the first fetch), the **刷新** button, which reads
**刷新中** while a batch is running, and the **置顶** button, which reads
**已置顶** while the panel is pinned. Dragging any blank part of the header
moves the window; the buttons still click.

**Cards.** One card per enabled provider, in a fixed order:

| Menu bar | Card |
| --- | --- |
| `ag` | Antigravity |
| `cc` | Claude Code |
| `cx` | Codex |
| `gk` | Grok Build |
| `gb` | Grok Bot |
| `oc` | OpenCode Go |

The large percentage on the right is the provider's tightest window — the one
that will run out first. Below it, each quota window gets a label, its own
percentage, a progress bar, and a reset time said in words (`1 天 21 小时后重置`,
`重置 11-16 08:00`, `当前不适用`). A final line carries credits or the expiry of
the login. When a fetch fails, the card shows the error in red and keeps the
last numbers it had.

**Footer.** The enabled provider shortcuts (`ag · cc · cx · gk · gb · oc`) and
**退出**.

## Color

| Remaining | Bar | Percentage |
| --- | --- | --- |
| 40–100% | Blue | Normal text |
| 15–39% | Amber | Normal text |
| 0–14% | Red | Red |
| Not applicable | Muted | Muted |

In the screenshot, Antigravity's weekly window is at 12% and reads red;
everything else is healthy blue.

## Size and scrolling

The window opens 344 pt wide and as tall as one 184 pt row per enabled
provider, plus 104 pt of chrome, clamped to 220–656 pt. With three or more
providers it opens 656 pt tall; content that does not fit scrolls inside the
sheet while the header and footer stay put.

The panel is resizable from any edge or corner, down to 280 × 220 pt. Its
contents follow the size: each card keeps a 240 pt basis and wraps into a
second (or third) column when the window is wide enough, and the progress
bars take a fraction of their card, so a wider panel reads side by side
instead of leaving empty space.

## Behaviour

- Opening the panel never fetches. It shows what is already in memory, so it
  appears instantly.
- On macOS, a second click on the tray item, a click in another app, or Escape
  closes it; the window's memory is released. Under GNOME, the tray label opens
  a menu and its **面板** item opens the panel.
- Data refreshes at launch, every five minutes, with the **刷新** button, or
  from the tray's **刷新** item. A provider turned back on refreshes right away.
- **置顶** pins the panel: it stays above other windows and outside clicks
  no longer dismiss it; clicking the tray icon brings it forward instead.
  Escape still closes it, and pressing **已置顶** releases the pin.
- `QUOTA_BAR_PIN_PANEL=1` opens the panel pinned for screenshots.
