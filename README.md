# Tzompantli

Tzompantli is a minimal application drawer for phones running Wayland.

# Demo

https://user-images.githubusercontent.com/8886672/161869929-f5a22a4d-ef9a-4c85-bfc5-ab8dafd3da0c.mp4

## Polkit

Tzompantli uses logind's DBus API to poweroff/reboot the system. To allow users
of the group `wheel` to suspend the system, the following polkit rule can be
added:

> /etc/polkit-1/rules.d/20-logind.rules

```
// Allow wheel users to poweroff.
polkit.addRule(function(action, subject) {
    if (action.id == "org.freedesktop.login1.poweroff" && subject.isInGroup("wheel")) {
        return "yes";
    }
});

// Allow wheel users to reboot.
polkit.addRule(function(action, subject) {
    if (action.id == "org.freedesktop.login1.reboot" && subject.isInGroup("wheel")) {
        return "yes";
    }
});
```
