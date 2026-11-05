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
