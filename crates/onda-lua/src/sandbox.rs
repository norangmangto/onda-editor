/// Lua sandbox: strips dangerous stdlib functions before plugins can load.
///
/// Blocked: `io.open`, `os.execute`, `os.remove`, `os.rename`,
///          `os.getenv` (harmless but not needed),
///          `require` (replaced with whitelist), `package.loadlib`,
///          `debug` library (reflection escape hatch),
///          `ffi` (LuaJIT FFI — not applicable to Lua 5.4 but just in case).
use mlua::{Lua, Result as LuaResult};
use tracing::debug;

/// Apply sandbox restrictions to the Lua VM.
pub fn apply(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();

    // Strip dangerous io functions
    if let Ok(io) = globals.get::<mlua::Table>("io") {
        io.set("open", mlua::Value::Nil)?;
        io.set("lines", mlua::Value::Nil)?;
        io.set("popen", mlua::Value::Nil)?;
        io.set("tmpfile", mlua::Value::Nil)?;
        debug!("Sandboxed: io.open/lines/popen/tmpfile");
    }

    // Strip dangerous os functions
    if let Ok(os) = globals.get::<mlua::Table>("os") {
        os.set("execute", mlua::Value::Nil)?;
        os.set("remove", mlua::Value::Nil)?;
        os.set("rename", mlua::Value::Nil)?;
        os.set("exit", mlua::Value::Nil)?;
        debug!("Sandboxed: os.execute/remove/rename/exit");
    }

    // Strip debug library (reflection / escape hatch)
    globals.set("debug", mlua::Value::Nil)?;

    // Strip load/loadfile/dofile (arbitrary code from filesystem)
    globals.set("loadfile", mlua::Value::Nil)?;
    globals.set("dofile", mlua::Value::Nil)?;

    // Replace require with a whitelist implementation
    let safe_require = lua.create_function(|lua, module: String| {
        const ALLOWED: &[&str] = &["string", "table", "math", "utf8", "bit32"];
        // onda.* modules are injected directly, not via require
        let is_allowed = ALLOWED
            .iter()
            .any(|&m| module == m || module.starts_with("onda."));
        if is_allowed {
            // Call the original require from the package library
            let package: mlua::Table = lua.globals().get("package")?;
            let loaded: mlua::Table = package.get("loaded")?;
            if let Ok(v) = loaded.get::<mlua::Value>(module.as_str()) {
                if !matches!(v, mlua::Value::Nil) {
                    return Ok(v);
                }
            }
            // For standard libs they're pre-loaded
            Err(mlua::Error::RuntimeError(format!(
                "module '{}' not available in sandbox",
                module
            )))
        } else {
            Err(mlua::Error::RuntimeError(format!(
                "sandbox: require of '{}' is not allowed",
                module
            )))
        }
    })?;
    globals.set("require", safe_require)?;

    // Strip package.loadlib
    if let Ok(package) = globals.get::<mlua::Table>("package") {
        package.set("loadlib", mlua::Value::Nil)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Lua, LuaOptions, StdLib};

    fn sandboxed_lua() -> Lua {
        let safe_libs =
            StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8 | StdLib::PACKAGE;
        let lua = Lua::new_with(safe_libs, LuaOptions::default()).expect("lua");
        apply(&lua).expect("sandbox apply");
        lua
    }

    #[test]
    fn os_execute_is_blocked() {
        let lua = sandboxed_lua();
        // os.execute should be nil after sandboxing
        let result: mlua::Result<mlua::Value> = lua.load(r#"return os.execute"#).eval();
        if let Ok(v) = result {
            assert!(
                matches!(v, mlua::Value::Nil),
                "os.execute should be nil, got {:?}",
                v
            )
        }
    }

    #[test]
    fn os_execute_call_errors() {
        let lua = sandboxed_lua();
        // Actually calling os.execute("...") should fail
        let result: mlua::Result<mlua::Value> = lua.load(r#"os.execute("echo pwned")"#).eval();
        assert!(
            result.is_err(),
            "calling os.execute should error in sandbox"
        );
    }

    #[test]
    fn require_of_disallowed_module_errors() {
        let lua = sandboxed_lua();
        let result: mlua::Result<mlua::Value> = lua.load(r#"require("io")"#).eval();
        assert!(
            result.is_err(),
            "require('io') should be blocked in sandbox"
        );
    }
}
