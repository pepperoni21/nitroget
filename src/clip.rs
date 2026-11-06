use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Clip;

    #[glib::object_subclass]
    impl ObjectSubclass for Clip {
        const NAME: &'static str = "NitrogetClip";
        type Type = super::Clip;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Clip {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn dispose(&self) {
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Clip {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.obj().first_child() {
                child.allocate(width, height, baseline, None);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.obj().first_child() {
                self.obj().snapshot_child(&child, snapshot);
            }
        }
    }
}

glib::wrapper! {
    pub struct Clip(ObjectSubclass<imp::Clip>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Clip {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn set_child(&self, child: &impl IsA<gtk::Widget>) {
        if let Some(previous) = self.first_child() {
            previous.unparent();
        }
        child.set_parent(self);
    }
}

impl Default for Clip {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_request_produces_exact_measure() {
        if gtk::init().is_err() {
            return;
        }

        let clip = Clip::new();
        clip.set_size_request(20, 12);

        let horizontal = clip.measure(gtk::Orientation::Horizontal, -1);
        let vertical = clip.measure(gtk::Orientation::Vertical, -1);

        assert_eq!((horizontal.0, horizontal.1), (20, 20));
        assert_eq!((vertical.0, vertical.1), (12, 12));
    }
}
