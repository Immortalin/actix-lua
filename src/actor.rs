use actix::prelude::*;
use actix::ActorContext;
use rlua::Error as LuaError;
use rlua::{FromLua, Function, Lua, MultiValue, ToLua, Value};

use std::cell::RefCell;
use std::collections::HashMap;
use std::str;
use std::time::Duration;
use uuid::Uuid;

use message::LuaMessage;

use builder::LuaActorBuilder;

/// Top level struct which holds a lua state for itself.
///
/// It provides most of the actix context API to the lua enviroment.
///
/// You can create new `LuaActor` with [`LuaActorBuilder`].
///
///
/// ### `ctx.msg`
/// The message sent to Lua actor.
///
/// ### `ctx.notify(msg)`
/// Send message `msg` to self.
///
/// ### `ctx.notify_later(msg, seconds)`
/// Send message `msg` to self after specified period of time.
///
/// ### `local recipient = ctx.new_actor(script_path, [actor_name])`
/// Create a new actor with given lua script. returns a recipient which can be used in `ctx.send` and `ctx.do_send`.
///
/// ### `local result = ctx.send(recipient, msg)`
/// Send message `msg` to `recipient asynchronously and wait for response.
///
/// Equivalent to `actix::Recipient.send`.
///
/// ### `ctx.do_send(recipient, msg)`
/// Send message `msg` to `recipient`.
///
/// Equivalent to `actix::Recipient.do_send`.
///
/// ### `ctx.terminate()`
/// Terminate actor execution.
///
/// [`LuaActorBuilder`]: struct.LuaActorBuilder.html
pub struct LuaActor {
    vm: Lua,
    pub recipients: HashMap<String, Recipient<LuaMessage>>,
}

impl LuaActor {

    pub fn new_with_vm( vm: Lua,
        started: Option<String>,
        handle: Option<String>,
        stopped: Option<String>,
    ) -> Result<LuaActor, LuaError> {

        let prelude = include_str!("lua/prelude.lua");
        vm.eval::<_, ()>(prelude, Some("Prelude"))?;
        {
            let load: Function = vm.globals().get("__load")?;
            if let Some(script) = started {
                let res = load.call::<(String, String), ()>((script, "started".to_string()));

                if let Err(e) = res {
                    return Result::Err(e);
                }
            }
            if let Some(script) = handle {
                let res = load.call::<(String, String), ()>((script, "handle".to_string()));

                if let Err(e) = res {
                    return Result::Err(e);
                }
            }
            if let Some(script) = stopped {
                let res = load.call::<(String, String), ()>((script, "stopped".to_string()));

                if let Err(e) = res {
                    return Result::Err(e);
                }
            }
        }

        Result::Ok(LuaActor {
            vm,
            recipients: HashMap::new(),
        })
    }

    pub fn new(
        started: Option<String>,
        handle: Option<String>,
        stopped: Option<String>,
    ) -> Result<LuaActor, LuaError> {
        let vm = Lua::new();
        Self::new_with_vm(vm, started, handle, stopped)
    }

    /// Add a recipient to the actor's recipient list.
    /// You can send message to the recipient via `name` with the context API `ctx.send(name, message)`
    pub fn add_recipients(
        &mut self,
        name: &str,
        rec: Recipient<LuaMessage>,
    ) -> Option<Recipient<LuaMessage>> {
        self.recipients.insert(name.to_string(), rec)
    }
}

// Remove all `self` usage with a independent function `invoke`.
fn invoke(
    self_addr: &Recipient<SendAttempt>,
    ctx: &mut Context<LuaActor>,
    vm: &mut Lua,
    recs: &mut HashMap<String, Recipient<LuaMessage>>,
    func_name: &str,
    args: Vec<LuaMessage>,
) -> Result<LuaMessage, LuaError> {
    // `ctx` is used in multiple closure in the lua scope.
    // to create multiple borrow in closures, we use RefCell to move the borrow-checking to runtime.
    // Voliating the check will result in panic. Which shouldn't happend(I think) since lua is single-threaded.
    let ctx = RefCell::new(ctx);
    let recs = RefCell::new(recs);

    let iter = args
        .into_iter()
        .map(|msg| msg.to_lua(&vm).unwrap())
        .collect();
    let args = MultiValue::from_vec(iter);
    // We can't create a function with references to `self` and is 'static since `self` already owns Lua.
    // A function within Lua owning `self` creates self-borrowing cycle.
    //
    // Also, Lua requires all values passed to it is 'static because we can't know when will Lua GC our value.
    // Therefore, we use scope to make sure these APIs are temporary and don't have to deal with 'static lifetime.
    //
    // (Quote from: https://github.com/kyren/rlua/issues/56#issuecomment-363928738
    // When the scope ends, the Lua function is 100% guaranteed (afaict!) to be "invalidated".
    // This means that calling the function will cause an immediate Lua error with a message like "error, call of invalidated function".)
    //
    // for reference, check https://github.com/kyren/rlua/issues/73#issuecomment-370222198
    vm.scope(|scope| {
        let globals = vm.globals();

        let notify = scope.create_function_mut(|_, msg: LuaMessage| {
            let mut ctx = ctx.borrow_mut();
            ctx.notify(msg);
            Ok(())
        })?;
        globals.set("notify", notify)?;

        let notify_later = scope.create_function_mut(|_, (msg, secs): (LuaMessage, u64)| {
            let mut ctx = ctx.borrow_mut();
            ctx.notify_later(msg, Duration::new(secs, 0));
            Ok(())
        })?;
        globals.set("notify_later", notify_later)?;

        let new_actor =
            scope.create_function_mut(|_, (script_path, name): (String, LuaMessage)| {
                let recipient_id = Uuid::new_v4();
                let mut recipient_name = format!("LuaActor-{}-{}", recipient_id, &script_path);
                if let LuaMessage::String(n) = name {
                    recipient_name = n;
                }

                let addr = LuaActorBuilder::new()
                    .on_handle(&script_path)
                    .build()?
                    .start();

                let mut recs = recs.borrow_mut();
                recs.insert(recipient_name.clone(), addr.recipient());
                Ok(recipient_name.clone())
            })?;
        globals.set("__new_actor", new_actor)?;

        let do_send =
            scope.create_function_mut(|_, (recipient_name, msg): (String, LuaMessage)| {
                let recs = recs.borrow_mut();
                let rec = recs.get(&recipient_name);

                // TODO: error handling?
                if let Some(r) = rec {
                    r.do_send(msg).unwrap();
                }
                Ok(())
            })?;
        globals.set("do_send", do_send)?;

        let send = scope.create_function_mut(
            |_, (recipient_name, msg, cb_thread_id): (String, LuaMessage, i64)| {
                // we can't create a lua function which owns `self`
                // but `self` is needed for resolving `send` future.
                //
                // The workaround is we notify ourself with a `SendAttempt` Message
                // and resolving `send` future in the `handle` function.
                self_addr
                    .do_send(SendAttempt {
                        recipient_name,
                        msg,
                        cb_thread_id,
                    }).unwrap();

                Ok(())
            },
        )?;
        globals.set("send", send)?;

        let terminate = scope.create_function_mut(|_, _: LuaMessage| {
            let mut ctx = ctx.borrow_mut();
            ctx.terminate();
            Ok(())
        })?;
        globals.set("terminate", terminate)?;

        let lua_handle: Result<Function, LuaError> = globals.get(func_name);
        if let Ok(f) = lua_handle {
            match f.call::<MultiValue, Value>(args) {
                Err(e) => panic!(e.to_string()),
                Ok(ret) => Ok(LuaMessage::from_lua(ret, &vm).unwrap()),
            }
        } else {
            // return nil if handle is not defined
            Ok(LuaMessage::Nil)
        }
    })
}

impl Actor for LuaActor {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Context<Self>) {
        if let Err(e) = invoke(
            &ctx.address().recipient(),
            ctx,
            &mut self.vm,
            &mut self.recipients,
            "__run",
            vec![LuaMessage::from("started")],
        ) {
            panic!("lua actor started failed {:?}", e);
        }
    }

    fn stopped(&mut self, ctx: &mut Context<Self>) {
        if let Err(e) = invoke(
            &ctx.address().recipient(),
            ctx,
            &mut self.vm,
            &mut self.recipients,
            "__run",
            vec![LuaMessage::from("stopped")],
        ) {
            panic!("lua actor stopped failed {:?}", e);
        }
    }
}

struct SendAttempt {
    recipient_name: String,
    msg: LuaMessage,
    cb_thread_id: i64,
}

impl Message for SendAttempt {
    type Result = LuaMessage;
}

struct SendAttemptResult {
    msg: LuaMessage,
    cb_thread_id: i64,
}

impl Message for SendAttemptResult {
    type Result = LuaMessage;
}

impl Handler<LuaMessage> for LuaActor {
    type Result = LuaMessage;

    fn handle(&mut self, msg: LuaMessage, ctx: &mut Context<Self>) -> Self::Result {
        if let Ok(res) = invoke(
            &ctx.address().recipient(),
            ctx,
            &mut self.vm,
            &mut self.recipients,
            "__run",
            vec![LuaMessage::from("handle"), msg],
        ) {
            res
        } else {
            LuaMessage::Nil
        }
    }
}

impl Handler<SendAttemptResult> for LuaActor {
    type Result = LuaMessage;

    fn handle(&mut self, result: SendAttemptResult, ctx: &mut Context<Self>) -> Self::Result {
        if let Ok(res) = invoke(
            &ctx.address().recipient(),
            ctx,
            &mut self.vm,
            &mut self.recipients,
            "__resume",
            vec![LuaMessage::from(result.cb_thread_id), result.msg],
        ) {
            res
        } else {
            LuaMessage::Nil
        }
    }
}

impl Handler<SendAttempt> for LuaActor {
    type Result = LuaMessage;

    fn handle(&mut self, attempt: SendAttempt, ctx: &mut Context<Self>) -> Self::Result {
        let rec = &self.recipients[&attempt.recipient_name];
        let self_addr = ctx.address().clone();
        rec.send(attempt.msg.clone())
            .into_actor(self)
            .then(move |res, _, _| {
                match res {
                    Ok(msg) => self_addr.do_send(SendAttemptResult {
                        msg,
                        cb_thread_id: attempt.cb_thread_id,
                    }),
                    _ => {
                        panic!("send attempt failed {:?}", res);
                    }
                };
                actix::fut::ok(())
            }).wait(ctx);

        LuaMessage::Nil
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_timer::Delay;
    use std::collections::HashMap;
    use std::time::Duration;
    use tokio::prelude::Future;

    use builder::LuaActorBuilder;

    fn lua_actor_with_handle(script: &str) -> LuaActor {
        LuaActorBuilder::new()
            .on_handle_with_lua(script)
            .build()
            .unwrap()
    }

    #[test]
    fn lua_actor_basic() {
        let system = System::new("test");

        let lua_addr = lua_actor_with_handle(r#"return ctx.msg + 1"#).start();

        let l = lua_addr.send(LuaMessage::from(1));
        Arbiter::spawn(
            l.map(|res| {
                assert_eq!(res, LuaMessage::from(2));
                System::current().stop();
            }).map_err(|e| println!("actor dead {}", e)),
        );

        system.run();
    }

    #[test]
    fn lua_actor_syntax_error() {
        let res = LuaActorBuilder::new()
            .on_handle_with_lua(r"return 1+")
            .build();

        if let Ok(_) = res {
            panic!("should return Err(syntax_error)");
        }
    }

    #[should_panic]
    #[test]
    fn lua_actor_user_error() {
        let system = System::new("test");

        let lua_addr = lua_actor_with_handle(
            r#"
        print("before")
        error("foo")
        print("after")
        "#,
        ).start();

        let l = lua_addr.send(LuaMessage::from(0));
        Arbiter::spawn(
            l.map(|_| {
                // it should panic
                System::current().stop();
            }).map_err(|e| println!("actor dead {}", e)),
        );

        system.run();
    }

    #[test]
    fn lua_actor_return_table() {
        let system = System::new("test");

        let lua_addr = lua_actor_with_handle(
            r#"
        return {x = 1}
        "#,
        ).start();

        let l = lua_addr.send(LuaMessage::from(3));
        Arbiter::spawn(
            l.map(|res| {
                let mut t = HashMap::new();
                t.insert("x".to_string(), LuaMessage::from(1));

                assert_eq!(res, LuaMessage::from(t));
                System::current().stop();
            }).map_err(|e| println!("actor dead {}", e)),
        );

        system.run();
    }

    #[test]
    fn lua_actor_state() {
        let system = System::new("test");

        let lua_addr = lua_actor_with_handle(
            r#"
        if not ctx.state.x then ctx.state.x = 0 end

        ctx.state.x = ctx.state.x + 1
        return ctx.state.x
        "#,
        ).start();

        let l = lua_addr.send(LuaMessage::Nil);
        Arbiter::spawn(
            l.map(move |res| {
                assert_eq!(res, LuaMessage::from(1));
                let l2 = lua_addr.send(LuaMessage::Nil);
                Arbiter::spawn(
                    l2.map(|res| {
                        assert_eq!(res, LuaMessage::from(2));
                        System::current().stop();
                    }).map_err(|e| println!("actor dead {}", e)),
                );
            }).map_err(|e| println!("actor dead {}", e)),
        );

        system.run();
    }

    #[test]
    fn lua_actor_notify() {
        let system = System::new("test");

        let addr = LuaActorBuilder::new()
            .on_started_with_lua(
                r#"
            ctx.notify(100)
            "#,
            ).on_handle_with_lua(
                r#"
            if ctx.msg == 100 then
                ctx.state.notified = ctx.msg
            end

            return ctx.msg + ctx.state.notified
            "#,
            ).build()
            .unwrap()
            .start();

        let delay = Delay::new(Duration::from_secs(1)).map(move |()| {
            let l = addr.send(LuaMessage::from(1));
            Arbiter::spawn(
                l.map(|res| {
                    assert_eq!(res, LuaMessage::from(101));
                    System::current().stop();
                }).map_err(|e| println!("actor dead {}", e)),
            )
        });
        Arbiter::spawn(delay.map_err(|e| println!("actor dead {}", e)));

        system.run();
    }

    #[test]
    fn lua_actor_notify_later() {
        let system = System::new("test");

        let addr = LuaActorBuilder::new()
            .on_started_with_lua(
                r#"
            ctx.notify_later(100, 1)
            "#,
            ).on_handle_with_lua(
                r#"
            if ctx.msg == 100 then
                ctx.state.notified = ctx.msg
            end

            return ctx.msg + ctx.state.notified
            "#,
            ).build()
            .unwrap()
            .start();

        let delay = Delay::new(Duration::from_secs(2)).map(move |()| {
            let l2 = addr.send(LuaMessage::from(1));
            Arbiter::spawn(
                l2.map(|res| {
                    assert_eq!(res, LuaMessage::from(101));
                    System::current().stop();
                }).map_err(|e| println!("actor dead {}", e)),
            )
        });
        Arbiter::spawn(delay.map_err(|e| println!("actor dead {}", e)));

        system.run();
    }

    #[test]
    fn lua_actor_rpc_new_actor() {
        let system = System::new("test");

        let addr = lua_actor_with_handle(
            r#"
        local id = ctx.new_actor("src/lua/test/test.lua")
        return id
        "#,
        ).start();
        let l = addr.send(LuaMessage::Nil);
        Arbiter::spawn(
            l.map(move |res| {
                if let LuaMessage::String(s) = res {
                    println!("{}", s);
                    assert!(s.ends_with("-src/lua/test/test.lua"));
                } else {
                    assert!(false);
                }

                System::current().stop();
            }).map_err(|e| println!("actor dead {}", e)),
        );

        system.run();
    }

    #[test]
    fn lua_actor_send() {
        let system = System::new("test");

        let addr = LuaActorBuilder::new()
            .on_started_with_lua(
                r#"
            local rec = ctx.new_actor("src/lua/test/test_send.lua", "child")
            ctx.state.rec = rec
            local result = ctx.send(rec, "Hello")
            print("new actor addr name", rec, result)
            "#,
            ).on_handle_with_lua(
                r#"
            return ctx.msg
            "#,
            ).build()
            .unwrap()
            .start();

        let delay = Delay::new(Duration::from_secs(1)).map(move |()| {
            let l = addr.send(LuaMessage::Nil);
            Arbiter::spawn(
                l.map(|res| {
                    assert_eq!(res, LuaMessage::Nil);
                    System::current().stop();
                }).map_err(|e| println!("actor dead {}", e)),
            )
        });
        Arbiter::spawn(delay.map_err(|e| println!("actor dead {}", e)));

        system.run();
    }

    #[test]
    fn lua_actor_thread_yield() {
        let system = System::new("test");

        let actor = LuaActorBuilder::new()
            .on_handle_with_lua(
                r#"
            local rec = ctx.new_actor("src/lua/test/test_send_result.lua", "child")
            ctx.state.rec = rec
            local result = ctx.send(rec, "Hello")
            print(result)
            return result
            "#,
            ).build()
            .unwrap();

        let addr = actor.start();

        let l = addr.send(LuaMessage::Nil);
        Arbiter::spawn(
            l.map(move |res| {
                if let LuaMessage::ThreadYield(_) = res {
                    System::current().stop();
                } else {
                    unimplemented!()
                }
            }).map_err(|e| println!("actor dead {}", e)),
        );

        system.run();
    }

    #[test]
    fn lua_actor_do_send() {
        // TODO: we're not really verifying the correctness of `do_send` here
        let system = System::new("test");

        let addr = LuaActorBuilder::new()
            .on_started_with_lua(
                r#"
            local rec = ctx.new_actor("src/lua/test/test_send.lua", "child")
            ctx.state.rec = rec
            local result = ctx.do_send(rec, "Hello")
            print("new actor addr name", rec, result)
            "#,
            ).on_handle_with_lua(
                r#"
            return ctx.msg
            "#,
            ).build()
            .unwrap()
            .start();

        let delay = Delay::new(Duration::from_secs(1)).map(move |()| {
            let l = addr.send(LuaMessage::Nil);
            Arbiter::spawn(
                l.map(|res| {
                    assert_eq!(res, LuaMessage::Nil);
                    System::current().stop();
                }).map_err(|e| println!("actor dead {}", e)),
            )
        });
        Arbiter::spawn(delay.map_err(|e| println!("actor dead {}", e)));

        system.run();
    }

    #[test]
    fn lua_actor_terminate() {
        // TODO: validate on_stopped is called
        let system = System::new("test");

        let _ = LuaActorBuilder::new()
            .on_started_with_lua(
                r#"
            ctx.terminate()
            "#,
            ).on_stopped_with_lua(r#"print("stopped")"#)
            .build()
            .unwrap()
            .start();
        let delay = Delay::new(Duration::from_secs(1)).map(move |()| {
            System::current().stop();
        });
        Arbiter::spawn(delay.map_err(|e| println!("actor dead {}", e)));

        system.run();
    }

    use std::env;

    #[test]
    fn lua_actor_require() {
        let system = System::new("test");
        env::set_var("LUA_PATH", "./src/?.lua;;");

        let addr = LuaActorBuilder::new()
            .on_handle_with_lua(
                r#"
                local m = require('lua/test/module')
                return m.incr(ctx.msg)
            "#,
            ).build()
            .unwrap()
            .start();
        let l = addr.send(LuaMessage::from(1));
        Arbiter::spawn(
            l.map(|res| {
                assert_eq!(res, LuaMessage::from(2));
                System::current().stop();
            }).map_err(|e| println!("actor dead {}", e)),
        );

        system.run();
    }

    #[test]
    fn lua_actor_with_vm() {
        let system = System::new("test");

        let vm = Lua::new();
        vm.globals().set("greet",
            vm.create_function( |_, name: String|
                Ok(format!("Hello, {}!", name))
            ).unwrap()
        ).unwrap();

        let addr = LuaActorBuilder::new()
            .on_handle_with_lua(
                r#"
            return greet(ctx.msg)
            "#,
            ).build_with_vm(vm)
            .unwrap()
            .start();

        let l = addr.send(LuaMessage::from("World"));
        Arbiter::spawn(
            l.map(|res| {
                assert_eq!(res, LuaMessage::from("Hello, World!"));
                System::current().stop();
            }).map_err(|e| println!("actor dead {}", e)),
        );

        system.run();
    }
}
