1. ~~https://github.com/pop-os/libcosmic#made-for-cosmic-flatpak-ids~~ (done: added `com.system76.CosmicApplet` id to metainfo.xml provides)
2. ~~remove the menu from the selection mode view~~ (done: popup closes immediately on Select Colour, reopens after colour selection)
   desired flow: click Select Colour → popup closes immediately → enter freeze/picker mode → colour selected → exit freeze mode → popup reopens showing selected colour
3. ~~flicker when entering selectioin mode~~ (done: pre-create transparent overlays before destroying popup, two-phase capture)
4. ~~make the magnifier distance half of what it is now from the mouse pointer.~~ (done: halved offset_x, offset_y, BELOW_OFFSET)
5. ~~respect system/libcosmic spacing, theme, frosted glass styles, fonts, colours everything.~~ (done: use app_popup + surface_task for blur, theme colors in magnifier, libcosmic spacing/corner_radii in popup)
6. ~~merge the 3 colour row in the popup menu into a single section still 3 rows but instead of 3 line separated sections simply 1 section for the colour values how is the cosmic design ethic for the menu popup in our case.~~ (done: single section with no dividers between rows, hide rows when no colour selected, caption_heading labels + monotext values, menu_button row with hover + click-to-copy)
7. ~~fix ai generated readme - look through existing readmes of other applets. especially system applets also the most popular 3rd party applets. also welcome pr's. like if other repos does.~~ (done: rewrote README following patterns from cosmic-ext-connected, system-monitor, next-meeting, classic-menu)
8. GitHub Pages + Aptly/ how does other applets ship/distribute?

9. Is code splitting needed?
10. Is higher res freeze mode available?
