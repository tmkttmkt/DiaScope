// SPDX-License-Identifier: MPL-2.0
#![doc = include_str!("../README.md")]

use std::sync::mpsc::Sender;
use std::sync::{Arc, LazyLock};
pub mod config_ui;

use config_ui::{ConfigUi, ReturnValueMap};
use paiagram_core::route::DiagramCache;
use paiagram_core::{Key, WorldSnapshot};
use paiagram_selection::SelectedItems;
use parking_lot::Mutex;
use rayon::spawn;
use rquickjs::class::Trace;
use rquickjs::{
    CatchResultExt, Class, Context, Ctx, Function, JsLifetime, Module, Object, Runtime, Value,
};

#[derive(Default, Debug)]
pub enum RunningStatus<T> {
    Running,
    Finished(rquickjs::Result<T>),
    #[default]
    Idle,
}

pub type ConfigRunningStatus = RunningStatus<Vec<ConfigUi>>;
pub type ScriptRunningStatus = RunningStatus<Option<WorldSnapshot>>;

static RQUICKJS_RUNTIME: LazyLock<Runtime> = LazyLock::new(|| Runtime::new().unwrap());

/// Evaluates the module and calls its exported `config_ui(world)` entrypoint. No [`JsIo`] is
/// created for this pass, so the configuration UI cannot trigger side effects.
pub fn eval_config(
    runtime_data: JsRuntimeData,
    status: Arc<Mutex<ConfigRunningStatus>>,
    script_str: Arc<str>,
) {
    *status.lock() = ConfigRunningStatus::Running;
    spawn(move || {
        let result = with_context(
            runtime_data,
            None,
            &script_str,
            |ctx, module, world, _io| {
                let entry: Function = module.get("config_ui")?;
                let value: Value = entry.call((world,))?;
                ConfigUi::parse_many(ctx, value)
            },
        );
        *status.lock() = ConfigRunningStatus::Finished(result);
    });
}

/// Runs the extension: evaluates the module and calls its exported `run(world, params, io)` entry
/// point, returning the new world if the script produced one. `params` holds the values collected
/// from the config UI. `io` is only available to `run`, so `config_ui` cannot trigger side effects.
pub fn run_script(
    runtime_data: JsRuntimeData,
    io: JsIo,
    return_values: ReturnValueMap,
    status: Arc<Mutex<ScriptRunningStatus>>,
    script_str: Arc<str>,
) {
    *status.lock() = ScriptRunningStatus::Running;
    spawn(move || {
        let result = with_context(
            runtime_data,
            Some(io),
            &script_str,
            |_ctx, module, world, io| {
                // `run` is optional: a script that only performs side effects (such as exporting a
                // file) doesn't have to export it, and then produces no new world.
                let entry: Option<Function> = module.get("run")?;
                let Some(entry) = entry else {
                    return Ok(None);
                };
                let io = io.expect("run always receives io");
                entry.call::<_, ()>((world.clone(), return_values, io))?;
                Ok(Some(std::mem::take(&mut world.borrow_mut().snap)))
            },
        );
        *status.lock() = ScriptRunningStatus::Finished(result);
    });
}

/// Evaluates `script_str` as an ES module in a fresh context, installs the global `console`, then
/// runs `f` with the module, the `world` argument passed to both entry points, and the optional
/// `io` argument passed only to `run`.
fn with_context<T>(
    runtime_data: JsRuntimeData,
    io: Option<JsIo>,
    script_str: &str,
    f: impl for<'js> FnOnce(
        &Ctx<'js>,
        Module<'js, rquickjs::module::Evaluated>,
        Class<'js, JsRuntimeData>,
        Option<Class<'js, JsIo>>,
    ) -> rquickjs::Result<T>,
) -> rquickjs::Result<T> {
    let ctx = Context::full(&RQUICKJS_RUNTIME).unwrap();
    ctx.with(|ctx| {
        install_console(&ctx)?;
        let world = Class::instance(ctx.clone(), runtime_data)?;
        let io = io.map(|io| Class::instance(ctx.clone(), io)).transpose()?;

        let script_error = |err: rquickjs::CaughtError<'_>| {
            rquickjs::Error::new_from_js_message("script", "extension", err.to_string())
        };
        let module = Module::declare(ctx.clone(), "extension.js", script_str)
            .catch(&ctx)
            .map_err(script_error)?;
        let (module, promise) = module.eval().catch(&ctx).map_err(script_error)?;
        promise.finish::<()>().catch(&ctx).map_err(script_error)?;

        f(&ctx, module, world, io).catch(&ctx).map_err(|err| {
            rquickjs::Error::new_from_js_message("entry point", "extension", err.to_string())
        })
    })
}

/// The read-only half of the extension API, exposed to both entry points as `world`.
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename_all = "camelCase")]
pub struct JsRuntimeData {
    #[qjs(skip_trace)]
    pub snap: WorldSnapshot,
    #[qjs(skip_trace)]
    pub selected_items: SelectedItems,
}

/// The side-effecting half of the extension API. It is passed to `run` as the third argument and
/// is intentionally not part of the `world` object given to `config_ui`.
#[derive(Trace, JsLifetime)]
#[rquickjs::class(rename_all = "camelCase")]
pub struct JsIo {
    // wasm only allows downloading files in the main thread
    #[qjs(skip_trace)]
    pub file_sender: Sender<JsDataExporter>,
}

pub struct JsDataExporter {
    pub filename: String,
    pub content: String,
}

impl paiagram_rw::ExportObject for JsDataExporter {
    fn write_content<W: std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(self.content.as_bytes())
    }
    fn filename(&self) -> String {
        self.filename.clone()
    }
}

#[rquickjs::methods(rename_all = "camelCase")]
impl JsRuntimeData {
    pub fn selected_items<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
        rquickjs_serde::to_value(ctx, &self.selected_items).map_err(|err| {
            rquickjs::Error::new_from_js_message(
                "selected_items",
                "rquickjs-serde",
                err.to_string(),
            )
        })
    }
    /// Returns every route key in the world. Accessible from scripts as `world.routeInfo()`.
    pub fn route_info<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
        let keys: Vec<_> = self.snap.routes.keys().copied().collect();
        rquickjs_serde::to_value(ctx, keys).map_err(|err| {
            rquickjs::Error::new_from_js_message("route_info", "rquickjs-serde", err.to_string())
        })
    }

    /// Returns a plain object mapping each trip key (as a decimal string) to its diagram lines.
    /// The cache is derived, so it is rebuilt from the world on each call. Accessible from
    /// scripts as `world.diagramCache()`.
    pub fn diagram_cache<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
        let caches = Object::new(ctx.clone())?;
        for route in self.snap.routes.values() {
            let mut cache = DiagramCache::default();
            route.intervals.populate_trips(&self.snap, &mut cache);
            for (trip_key, polylines) in &cache.map {
                let value = rquickjs_serde::to_value(ctx.clone(), polylines).map_err(|err| {
                    rquickjs::Error::new_from_js_message(
                        "diagram_cache",
                        "rquickjs-serde",
                        err.to_string(),
                    )
                })?;
                caches.set(trip_key.to_bits().to_string(), value)?;
            }
        }
        Ok(caches.into_value())
    }
}

#[rquickjs::methods(rename_all = "camelCase")]
impl JsIo {
    /// Request a file download from JS. Only available to `run` as `io.saveFile(...)`.
    pub fn save_file(&mut self, filename: String, extension: String, content: String) {
        self.file_sender
            .send(JsDataExporter {
                filename: format!("{filename}{extension}"),
                content,
            })
            .unwrap();
    }
}

/// JavaScript factory for the global `console` object. It takes the Rust log bridge as a
/// parameter, so no extra globals are needed.
const CONSOLE_SHIM: &str = r#"
((log) => {
    const fmt = (value) => {
        if (typeof value === "string") return value;
        if (value === null) return "null";
        if (value === undefined) return "undefined";
        if (typeof value === "object") {
            try { return JSON.stringify(value); } catch (_) { return String(value); }
        }
        return String(value);
    };
    const make = (level) => (...args) => log(level, args.map(fmt).join(" "));
    return {
        log: make(1),
        info: make(1),
        debug: make(2),
        warn: make(3),
        error: make(4),
        trace: make(5),
    };
})
"#;

/// Installs the standard `console` global, whose methods forward to the `log` crate.
fn install_console<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<()> {
    let log_fn = Function::new(ctx.clone(), |level: u32, message: String| {
        let level = match level {
            2 => log::Level::Debug,
            3 => log::Level::Warn,
            4 => log::Level::Error,
            5 => log::Level::Trace,
            _ => log::Level::Info,
        };
        log::log!(target: "script", level, "{message}");
    })?;
    let factory: Function = ctx.eval(CONSOLE_SHIM)?;
    let console: Object = factory.call((log_fn,))?;
    ctx.globals().set("console", console)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use paiagram_core::TripKey;
    use paiagram_selection::SelectedItem;
    use rquickjs::{Array, BigInt, Context, Runtime, Value};

    use super::*;

    #[test]
    fn selected_items_serialize_keys_exactly() {
        // Allocate a key with nonzero counter bits so f64 precision loss would be visible.
        let _ = TripKey::new();
        let trip = TripKey::new();
        let data = JsRuntimeData {
            snap: WorldSnapshot::default(),
            selected_items: SelectedItem::Trip(trip).into(),
        };

        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        context.with(|ctx| {
            let value: Value = data.selected_items(ctx.clone()).unwrap();
            let object = value.into_object().unwrap();
            let trips: Array = object.get("Trips").unwrap();
            let first: BigInt = trips.get(0).unwrap();
            assert_eq!(first.to_i64().unwrap() as u64, trip.to_bits());
        });
    }
}
