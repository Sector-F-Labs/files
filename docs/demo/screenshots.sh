#!/bin/bash
# Makes every website screenshot in ../screenshots. Run through `make screenshots`.
cd "$(dirname "$0")"
SIZE=1280x1190 ./shot.sh files-zfs /home/demo/Pictures 'view = "Icons"
zfs_panel = true
skin = "Everforest Dark"
light_dark = "dark"'
./shot.sh files-apps /home/demo/Apps 'view = "Icons"
skin = "Everforest Dark"
light_dark = "dark"'
SIZE=1280x640 ./shot.sh files-list /home/demo/Documents 'view = "List"
skin = "Everforest Dark"
light_dark = "dark"
list_columns = ["Size", "Modified", "Kind"]'
./shot.sh files-columns /home/demo/Projects/lantern/src 'view = "Columns"
skin = "Everforest Dark"
light_dark = "dark"'
./shot.sh files-pictures /home/demo/Pictures 'view = "Icons"
skin = "Everforest Dark"
light_dark = "dark"'
