use gpui_kit::{Role, SharedString, StatefulInteractiveElement, Toggled};

/// Semantic labels and states for custom controls. The view owns each control's actions.
pub(crate) trait A11yElementExt: StatefulInteractiveElement {
    fn a11y_label(
        self,
        role: Role,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> Self {
        self.role(role).accessibility_id(id).aria_label(label)
    }

    fn a11y_button(self, id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        self.a11y_label(Role::Button, id, label)
    }

    fn a11y_menuitem(self, id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        self.a11y_label(Role::MenuItem, id, label)
    }

    fn a11y_link(self, id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        self.a11y_label(Role::Link, id, label)
    }

    fn a11y_tab(
        self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        selected: bool,
    ) -> Self {
        self.a11y_label(Role::Tab, id, label)
            .aria_selected(selected)
    }

    fn a11y_listitem(
        self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        selected: bool,
    ) -> Self {
        self.a11y_label(Role::ListItem, id, label)
            .aria_selected(selected)
    }

    fn a11y_treeitem(
        self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        selected: bool,
    ) -> Self {
        self.a11y_label(Role::TreeItem, id, label)
            .aria_selected(selected)
    }

    fn a11y_checkbox(
        self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        checked: bool,
    ) -> Self {
        self.a11y_label(Role::CheckBox, id, label)
            .aria_toggled(toggled(checked))
    }

    fn a11y_toggle_button(
        self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        pressed: bool,
    ) -> Self {
        self.a11y_label(Role::Button, id, label)
            .aria_toggled(toggled(pressed))
    }

    fn a11y_region(self, id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        self.a11y_label(Role::Region, id, label)
    }

    fn a11y_nav(self, id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        self.a11y_label(Role::Navigation, id, label)
    }

    fn a11y_dialog(self, id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        self.a11y_label(Role::Dialog, id, label)
    }

    fn a11y_status(self, id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        self.a11y_label(Role::Status, id, label)
    }

    fn a11y_image(self, id: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        self.a11y_label(Role::Image, id, label)
    }
}

impl<T: StatefulInteractiveElement> A11yElementExt for T {}

fn toggled(active: bool) -> Toggled {
    if active {
        Toggled::True
    } else {
        Toggled::False
    }
}
