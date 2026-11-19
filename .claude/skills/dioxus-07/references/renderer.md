# Writing a custom renderer (Dioxus 0.7.10)

Scope: implementing a renderer against `dioxus-core` (a new target that is not web, desktop,
liveview or native). For *using* the existing Blitz native renderer, use the `blitz-native` skill.

Built from three official sources, with the API pinned to the `v0.7.10` tag:
- the 0.7 Custom Renderer guide (concepts; some code in it is stale, see "Drift" below)
- `AGENTS.md` and `notes/architecture/{01-CORE,06-RENDERERS}.md` at tag `v0.7.10`
- `packages/core/src/{mutations,virtual_dom}.rs` at tag `v0.7.10`

A renderer only has to do two things: **apply the stream of edits** the VirtualDom produces, and
**feed events back**. Tree relationships, diffing, memory management and the event system stay
inside `dioxus-core`.

## VirtualDom API (0.7.10)

```rust
let mut vdom = VirtualDom::new(app);                 // app: fn() -> Element
// or VirtualDom::new_with_props(app, props); vdom.with_root_context(ctx)

vdom.rebuild(&mut writer);                           // initial render into a WriteMutations sink
vdom.rebuild_in_place();                             // initial render, discard mutations
let muts = vdom.rebuild_to_vec();                    // -> Mutations { edits: Vec<Mutation> }

loop {
    vdom.wait_for_work().await;                      // futures, scheduler, dirty scopes
    vdom.render_immediate(&mut writer);              // diff dirty scopes, emit mutations
    // or: let muts = vdom.render_immediate_to_vec();
}

vdom.handle_event(name, Rc<dyn Any>, element_id, bubbling);   // feed a platform event
vdom.wait_for_suspense().await;                      // SSR-style: block until suspense resolves
vdom.runtime();                                      // Rc<Runtime>
```

`render_immediate` is called when you want a frame (for the native renderer: on `RedrawRequested`).
`handle_event` takes an `Rc<dyn Any>` that your `HtmlEventConverter` implementation can downcast.

## `WriteMutations` (exact trait, 0.7.10)

```rust
pub trait WriteMutations {
    fn append_children(&mut self, id: ElementId, m: usize);
    fn assign_node_id(&mut self, path: &'static [u8], id: ElementId);
    fn create_placeholder(&mut self, id: ElementId);
    fn create_text_node(&mut self, value: &str, id: ElementId);
    fn load_template(&mut self, template: Template, index: usize, id: ElementId);
    fn replace_node_with(&mut self, id: ElementId, m: usize);
    fn replace_placeholder_with_nodes(&mut self, path: &'static [u8], m: usize);
    fn insert_nodes_after(&mut self, id: ElementId, m: usize);
    fn insert_nodes_before(&mut self, id: ElementId, m: usize);
    fn set_attribute(&mut self, name: &'static str, ns: Option<&'static str>,
                     value: &AttributeValue, id: ElementId);
    fn set_node_text(&mut self, value: &str, id: ElementId);
    fn create_event_listener(&mut self, name: &'static str, id: ElementId);
    fn remove_event_listener(&mut self, name: &'static str, id: ElementId);
    fn remove_node(&mut self, id: ElementId);
    fn push_root(&mut self, id: ElementId);
}
```

Semantics, from the trait's own docs:
- It is a **stack machine**. `load_template`, `create_placeholder`, `create_text_node` push a real
  node; `append_children(id, m)`, `insert_nodes_after/before`, `replace_node_with`,
  `replace_placeholder_with_nodes` consume `m` nodes from the stack.
- `path` in `assign_node_id` / `replace_placeholder_with_nodes` is a list of child indices from the
  node on top of the stack (`[]` is that node, `[0,1]` is first child's second child). Templates are
  limited to 255 children per element (one byte).
- `ElementId(0)` is the root; the renderer must remember id to node. A sparse `Vec<Option<T>>`
  indexed by id works; Dioxus reclaims ids on removal.
- `Mutation` is a serialisable enum mirror of these calls (for tests, or to ship edits across a
  network); variants: `AppendChildren`, `AssignId`, `CreatePlaceholder`, `CreateTextNode`,
  `LoadTemplate { index, id }`, `ReplaceWith`, `ReplacePlaceholder`, `InsertAfter`, `InsertBefore`,
  `SetAttribute`, `SetText`, `NewEventListener`, `RemoveEventListener`, `Remove`, `PushRoot`.
- `Mutations` (what `*_to_vec()` returns) holds only `edits: Vec<Mutation>`. `Mutation::LoadTemplate`
  has no template payload: the `Template` arrives through `WriteMutations::load_template`, so if
  you need templates, implement `WriteMutations` yourself instead of collecting `Mutations`.
- `load_template` passes the `Template` on every call. Build the static node once per template and
  clone it afterwards (the web renderer keeps a `HashMap<Template, u16>`).

## Templates

```rust
pub struct Template {
    pub roots: &'static [TemplateNode],
    pub node_paths: &'static [&'static [u8]],
    pub attr_paths: &'static [&'static [u8]],
}
// TemplateNode: Element { tag, namespace, attrs, children } | Text { text } | Dynamic { id }
// DynamicNode:  Component(VComponent) | Text(VText) | Placeholder(VPlaceholder) | Fragment(Vec<VNode>)
```

A template is the compile-time-known tree for an `rsx!` block, with holes for dynamic parts. The
diff skips static nodes, and the renderer can build the static part once and clone it per list item.

Worked example from the guide (concept is right; one name is stale): for `rsx! { h1 { "count: {x}" } }`
the stream is a template load (push `<h1>""</h1>`), a text set for the dynamic text (`"count: 0"`),
then `AppendChildren { id: 0, m: 1 }` to attach it to the root. Stack over time: `[Root]`,
`[Root, h1]`, `[Root, h1("count: 0")]`, `[Root]`. The guide names the text step `HydrateText`, which
is not a `Mutation` variant in 0.7.10 (see the variant list above).

## Event loop shape

1. Create the VirtualDom, create the real root and register it as `ElementId(0)`.
2. `rebuild(&mut writer)` and apply what it wrote.
3. Loop: select between `vdom.wait_for_work()` and your platform events. On work, call
   `render_immediate(&mut writer)` and flush. On a platform event, convert it with your
   `HtmlEventConverter` and call `handle_event`.
4. `create_event_listener` / `remove_event_listener` tell you which events to subscribe to per node.
   The web renderer uses one delegated listener on the root and walks up to a `data-dioxus-id`.

## What the first-party renderers do (AGENTS notes, 0.7.10)

| Renderer | How it implements `WriteMutations` | Loop |
|---|---|---|
| web | calls the Sledgehammer JS interpreter through wasm-bindgen; templates cached JS-side | browser event loop |
| desktop | batches mutations, sends over a websocket to the webview (binary protocol) | tao event loop |
| liveview | batches and ships binary edits over a websocket; VirtualDom runs on the server | tokio `select!` |
| native | applies directly to the Blitz DOM, then layout, then Vello draws | winit `ApplicationHandler` |

Every renderer also supplies: an `HtmlEventConverter` (platform event data to typed Dioxus event
data), a launch function (`Box<dyn Any>` configs downcast per renderer), and optionally history,
document, and asset serving. To make `dioxus` accept an out-of-tree renderer without warnings, use
the `third-party-renderer` feature.

Custom elements/attributes: HTML and SVG attributes go through helper structs in `dioxus_html`; a
renderer can add its own namespace (see `packages/html/README.md`, "How to extend it").

## Drift: what the guide says that 0.7.10 does not

| Guide text | 0.7.10 reality |
|---|---|
| `Template { name: "main.rs:1:1:0", .. }` | `name` field removed in 0.6 |
| `TemplateNode::DynamicText { id }` | variants are `Element`, `Text`, `Dynamic` |
| `Mutation` list with `HydrateText` / `CreateElement`; `rebuild()` returns mutations | variants listed above; use `rebuild(&mut impl WriteMutations)` or `rebuild_to_vec()` |
| `UserEvent { scope_id, priority, .. }` | events go through `handle_event(name, Rc<dyn Any>, ElementId, bubbling)` |
| `work_with_deadline(|| false)` | `render_immediate` |
| links to `v0.5` interpreter and a `dioxus-tui` renderer | stale; read `packages/interpreter` and `packages/native` on the `v0.7.10` tag |

## Re-check on upgrade to 0.8

`main` is `0.8.0-alpha.3`. Already visible drift in `notes/architecture/01-CORE.md`:
`Properties::builder()` became `component_builder(render_fn)`, `VNode` mount state moved to a mount
arena (`MountedVNode`), `DynamicValues` was added. Re-read `mutations.rs` and `virtual_dom.rs` at
the new tag before porting a renderer; do not assume the trait above is unchanged.
