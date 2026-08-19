use conquer_once::spin::OnceCell;

use crate::limine::EXECUTABLE_CMDLINE_REQUEST;

static CMDLINE: OnceCell<Cmdline> = OnceCell::uninit();

pub(crate) fn init() {
    CMDLINE.init_once(|| {
        EXECUTABLE_CMDLINE_REQUEST
            .get_response()
            .and_then(|resp| resp.cmdline().to_str().ok())
            .expect("should be able to read cmdline")
            .into()
    })
}

pub fn cmdline() -> &'static Cmdline<'static> {
    CMDLINE.try_get().expect("should have cmdline")
}

pub struct Cmdline<'a> {
    cmdline: &'a str,
}

impl<'a> From<&'a str> for Cmdline<'a> {
    fn from(v: &'a str) -> Self {
        Self { cmdline: v }
    }
}

impl<'a> Cmdline<'a> {
    fn pairs(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.cmdline
            .split_ascii_whitespace()
            .map(|v| v.split('='))
            .filter_map(|mut v| Some((v.next()?, v.next())))
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.pairs()
            .filter(|p| p.0 == key)
            .map(|p| p.1)
            .next()
            .flatten()
    }
}

macro_rules! arg {
    ($($name:ident : $key:literal),*,) => {
        impl<'a> Cmdline<'a> {
            $(
                pub fn $name(&self) -> Option<&str> {
                    self.get($key)
                }
            )*
        }
    };
}

arg! {
    rust_log: "RUST_LOG",
    init: "init",
}
