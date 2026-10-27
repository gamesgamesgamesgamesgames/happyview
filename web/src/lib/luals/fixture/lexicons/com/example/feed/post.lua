---@meta

--- A post
---@class lexicons.com.example.feed.post.Record
---@field createdAt string
---@field embed? lexicons.com.example.feed.post.image|lexicons.com.example.actor.defs.profileView|table
---@field extra? table<string, any>
---@field kind? "text"|"link"|string
---@field langs? string[]|nil
---@field pinned? happyview.CidLink
---@field raw? happyview.Bytes
---@field reply? lexicons.com.example.feed.post.replyRef
---@field text string The post's text
---@field visibility? "public"|"private"

--- What a record-event script for this collection receives
---@class lexicons.com.example.feed.post.Event
---@field action "create"|"update"|"delete"
---@field uri string The record's AT URI
---@field did string The repo the record is in
---@field collection string The record's collection NSID
---@field rkey string The record's key
---@field record lexicons.com.example.feed.post.Record|nil The record body; null for a delete

---@class lexicons.com.example.feed.post.image
---@field alt? string
---@field image happyview.Blob

---@class lexicons.com.example.feed.post.replyRef
---@field parent? any
---@field root lexicons.com.example.actor.defs.strongRef

