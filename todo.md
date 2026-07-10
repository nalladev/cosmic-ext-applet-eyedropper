1. ~~https://github.com/pop-os/libcosmic#made-for-cosmic-flatpak-ids~~ (done: added `com.system76.CosmicApplet` id to metainfo.xml provides)
2. ~~remove the menu from the selection mode view~~ (done: popup closes immediately on Select Colour, reopens after colour selection)
   desired flow: click Select Colour → popup closes immediately → enter freeze/picker mode → colour selected → exit freeze mode → popup reopens showing selected colour
3. ~~flicker when entering selectioin mode~~ (done: pre-create transparent overlays before destroying popup, two-phase capture)
4. ~~make the magnifier distance half of what it is now from the mouse pointer.~~ (done: halved offset_x, offset_y, BELOW_OFFSET)
5. respect system/libcosmic spacing, theme, frosted glass styles, fonts, colours everything.
6. fix ai generated readme - look through existing readmes of other applets.
7. GitHub Pages + Aptly/ how does other applets ship/distribute?
