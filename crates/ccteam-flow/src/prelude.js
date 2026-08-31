// prelude.js — the workflow script's entire world.
//
// Runs once, before the script, in the same realm. It does three things:
//
//   1. Traps the APIs that would make a run unrepeatable (wall clock,
//      randomness). These traps are AUTHORITATIVE: the Rust-side source scan
//      is only an early, readable warning and cannot see `Date["now"]()`.
//   2. Wraps the Rust host primitives (prefixed `__ccteam_`) into the six
//      script-facing globals: agent / parallel / pipeline / phase / log /
//      usage, plus the `args` and `budget` values.
//   3. Deletes the raw primitives from the global object, so the script can
//      only reach the host through the wrappers it is documented to use.
//
// Threat model, stated honestly: this is COOPERATIVE determinism, not a
// sandbox. There is no module loader, no filesystem, no network and no
// process object in this realm — those simply do not exist in the embedded
// engine — but a script that deliberately hunts for a back channel is out of
// scope. The traps exist so a well-meaning author cannot break resume by
// accident.
(function () {
  var g = globalThis;

  var __agent_raw = g.__ccteam_agent;
  var __phase_raw = g.__ccteam_phase;
  var __log_raw = g.__ccteam_log;
  var __spent_raw = g.__ccteam_spent;
  var __usage_raw = g.__ccteam_usage;
  var __budget_total = g.__ccteam_budget_total;
  var __args_json = g.__ccteam_args_json;
  var BLOCKED = g.__ccteam_blocked_msg;

  delete g.__ccteam_agent;
  delete g.__ccteam_phase;
  delete g.__ccteam_log;
  delete g.__ccteam_spent;
  delete g.__ccteam_usage;
  delete g.__ccteam_budget_total;
  delete g.__ccteam_args_json;
  delete g.__ccteam_blocked_msg;

  function boom() {
    throw new Error(BLOCKED);
  }
  function hide(obj, name, value) {
    Object.defineProperty(obj, name, {
      value: value,
      writable: false,
      enumerable: false,
      configurable: false,
    });
  }

  // ── randomness ───────────────────────────────────────────────────────────
  hide(Math, 'random', boom);

  // ── wall clock ───────────────────────────────────────────────────────────
  // Explicit-argument Date stays fully usable: `new Date(2026, 0, 1)` is a
  // pure function of its inputs. Only the now-reading paths are closed.
  //
  // The sanitised prototype matters as much as the constructor: without it,
  // `new Date(0).constructor.now()` walks straight back to the real Date and
  // the guarantee is gone. Instances are built with Reflect.construct (so
  // they keep Date's internal slot and all its methods) but re-homed onto a
  // prototype whose own [[Prototype]] is Object.prototype, so the real Date
  // is nowhere on the chain.
  var RealDate = Date;
  var SafeDate = function () {
    if (!new.target) {
      // Bare `Date()` only means anything against the wall clock.
      throw new Error(BLOCKED);
    }
    if (arguments.length === 0) {
      throw new Error(BLOCKED);
    }
    return Reflect.construct(RealDate, Array.prototype.slice.call(arguments), SafeDate);
  };
  var safeProto = Object.create(Object.prototype);
  Reflect.ownKeys(RealDate.prototype).forEach(function (k) {
    if (k === 'constructor') return;
    var d = Object.getOwnPropertyDescriptor(RealDate.prototype, k);
    if (d) Object.defineProperty(safeProto, k, d);
  });
  Object.defineProperty(safeProto, 'constructor', {
    value: SafeDate,
    writable: true,
    enumerable: false,
    configurable: true,
  });
  Object.defineProperty(SafeDate, 'prototype', {
    value: safeProto,
    writable: false,
    enumerable: false,
    configurable: false,
  });
  SafeDate.parse = RealDate.parse.bind(RealDate);
  SafeDate.UTC = RealDate.UTC.bind(RealDate);
  hide(SafeDate, 'now', boom);
  hide(g, 'Date', SafeDate);

  // ── locale date formatting ───────────────────────────────────────────────
  // `Intl.DateTimeFormat().format()` defaults its argument to the system
  // clock inside the engine, bypassing the Date trap entirely. The whole
  // class is date/time formatting, so it is replaced wholesale; Intl's
  // time-independent members (NumberFormat, Collator) are carried over if the
  // engine build has them.
  var RealIntl = typeof g.Intl === 'object' && g.Intl !== null ? g.Intl : {};
  var SafeIntl = {};
  Object.getOwnPropertyNames(RealIntl).forEach(function (k) {
    SafeIntl[k] = RealIntl[k];
  });
  SafeIntl.DateTimeFormat = boom;
  hide(g, 'Intl', Object.freeze(SafeIntl));

  // ── host bridge ──────────────────────────────────────────────────────────
  // Every host reply is a JSON envelope: {k:'ok', v:<value>} or
  // {k:'throw', m:<message>}. Rust decides which failures are the script's
  // problem (unknown option, tripped brake -> throw) and which are the
  // worker's (failed hire, refusal -> v:null); JS just obeys.
  function unwrap(raw) {
    var env = JSON.parse(raw);
    if (env.k === 'throw') {
      throw new Error(env.m);
    }
    return env.v;
  }

  function agent(task, opts) {
    var encoded;
    try {
      encoded = JSON.stringify(opts === undefined ? null : opts);
    } catch (e) {
      throw new Error('agent() options must be plain JSON data: ' + String(e));
    }
    return __agent_raw(String(task), encoded === undefined ? 'null' : encoded).then(unwrap);
  }

  // BARRIER. Every thunk settles; a thrower becomes null in its slot so the
  // caller can `.filter(Boolean)` instead of wrapping each call in try/catch.
  function parallel(thunks) {
    if (!Array.isArray(thunks)) {
      throw new Error('parallel(thunks) expects an array of () => Promise');
    }
    return Promise.all(
      thunks.map(function (t) {
        if (typeof t !== 'function') {
          return Promise.resolve(null);
        }
        return Promise.resolve()
          .then(t)
          .then(null, function () {
            return null;
          });
      })
    );
  }

  // ITEM-STREAMING, no barrier between stages: item A can be in stage 3 while
  // item B is still in stage 1. This is the default the official contract
  // asks for, and the reason a fan-out's wall clock is the slowest single
  // chain rather than the sum of per-stage maxima.
  //
  // SEAM (F0b+): a `stage-barrier` strategy — await all items at each stage
  // boundary — plugs in here as an option; only streaming is implemented, so
  // a script that genuinely needs cross-item context uses parallel().
  function pipeline(items) {
    if (!Array.isArray(items)) {
      throw new Error('pipeline(items, ...stages) expects an array of items');
    }
    var stages = Array.prototype.slice.call(arguments, 1);
    var chains = items.map(function (item, idx) {
      var p = Promise.resolve(item);
      stages.forEach(function (st) {
        p = p.then(function (prev) {
          if (typeof st !== 'function') {
            throw new Error('pipeline() stages must be functions');
          }
          return st(prev, item, idx);
        });
      });
      // A throwing stage drops this item to null and skips the rest of ITS
      // chain; other items are unaffected.
      return p.then(null, function () {
        return null;
      });
    });
    return Promise.all(chains);
  }

  function phase(title) {
    __phase_raw(String(title));
  }

  function log(message) {
    __log_raw(String(message));
  }

  function usage() {
    return __usage_raw().then(unwrap);
  }

  var budget = Object.freeze({
    total: __budget_total === undefined || __budget_total === null ? null : __budget_total,
    spent: function () {
      return __spent_raw();
    },
    remaining: function () {
      if (budget.total === null) return Infinity;
      var left = budget.total - __spent_raw();
      return left > 0 ? left : 0;
    },
  });

  var args = __args_json === undefined ? undefined : JSON.parse(__args_json);

  g.agent = agent;
  g.parallel = parallel;
  g.pipeline = pipeline;
  g.phase = phase;
  g.log = log;
  g.usage = usage;
  g.budget = budget;
  g.args = args;
})();
