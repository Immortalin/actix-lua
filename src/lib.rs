//! # actix-lua: Lua scripting for actix
//!
//! The `actix-lua` crate provides a safe [Lua programming language] scripting enviroment for [actix], an actor framework.
//!
//! # The `LuaActor` object
//!
//! The main type exported by this library is the [`LuaActor`] struct.
//!
//! You should create [`LuaActor`] with [`LuaActorBuilder`].
//!
//! ```
//! extern crate actix_lua;
//! extern crate actix;
//!
//! use actix_lua::{LuaActorBuilder};
//! use actix::Actor;
//!
//! let addr = LuaActorBuilder::new()
//!     .on_handle_with_lua(r#"return ctx.msg + 42"#)
//!     .build()
//!     .unwrap()
//!     .start();
//! ```
//!
//! # The `LuaMessage` type
//!
//! [`LuaActor`] can only send/receive messages with type [`LuaMessage`].
//! It can be converted from/to primitive types such as `i64`, `String`, and `HashMap` with `LuaMessage::from`.
//!
//! [actix]: https://github.com/actix/actix
//! [Lua programming language]: https://www.lua.org
//! [`LuaActor`]: struct.LuaActor.html
//! [`LuaActorBuilder`]: struct.LuaActorBuilder.html
//! [`LuaMessage`]: enum.LuaMessage.html
extern crate actix;
extern crate regex;
extern crate rlua;
extern crate tokio;
extern crate uuid;

#[cfg(test)]
extern crate futures_timer;

mod actor;
mod builder;
mod message;

pub use actor::LuaActor;
pub use builder::LuaActorBuilder;
pub use message::LuaMessage;

pub mod dev {
    /// Re-export `rlua` interface for library developers
    pub mod rlua {
        pub use rlua::*;
    }
}
