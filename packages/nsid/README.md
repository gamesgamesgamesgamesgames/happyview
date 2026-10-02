# @happyview/nsid

AT Protocol NSID validation, for consumers of [HappyView](https://github.com/gamesgamesgamesgamesgames/happyview).

This package owns the spec's canonical regex verbatim rather than pulling in a full atproto SDK for three lines of validation logic. It's the TypeScript twin of the `happyview-nsid` Rust crate — the same interop corpus pins both, so `NSID_PATTERN` is byte-identical between them.

## Installation

```bash
npm install @happyview/nsid
```

## Usage

```typescript
import { isValidNsid, assertValidNsid, nsidAuthority, NSID_PATTERN, MAX_NSID_LEN } from "@happyview/nsid";

isValidNsid("com.example.feed.getHot"); // true
isValidNsid("2bit.pics.photo"); // false — TLD must start with a letter

assertValidNsid("2bit.pics.photo"); // throws InvalidNsidError

nsidAuthority("com.example.feed.getHot"); // "feed.example.com"
```

Only the first (TLD) and last (name) segments must start with a letter; the authority segments between them are reversed domain labels and may start with a digit — for example `pics.2bit.feed.getPhotos` is a valid NSID.

`NSID_PATTERN` is exported as a string rather than a `RegExp` so it can be dropped directly into a JSON Schema `pattern` field. `MAX_NSID_LEN` (317) bounds the total length, which the pattern itself does not.
