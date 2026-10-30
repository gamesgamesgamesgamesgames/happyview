A project whose lexicon the spec checks refuse but earlier HappyViews
accepted: `#author` names a def that is not defined.

It exists to pin that upgrading cannot stop a boot. Boot builds this
leniently and warns; `happyview check` and a deploy refuse it.
