// Copy for the happyview.dev landing page. Copy only: the page's layout and
// components are the docs site's. Every claim is sourced in CLAIMS.md beside
// this file; the sample files under sample/ are a project that ran on
// HappyView 3.0.0-alpha.42 and produced the responses below.
import toml from './sample/happyview.toml?raw'
import lexicon from './sample/src/lexicons/com/example/hello.json?raw'
import script from './sample/src/lexicons/com/example/hello.lua?raw'
import test from './sample/src/lexicons/com/example/hello.test.lua?raw'

export const hero = {
	title: 'Write the lexicon. HappyView serves the API.',
	subline:
		'HappyView is an open-source AppView for atproto. Each lexicon you add becomes an XRPC endpoint, a short script in Lua, JavaScript or TypeScript decides what it answers, and it indexes the records your app cares about as they land on the network.',
	ctas: {
		primary: { label: 'Start a project', href: '/getting-started/' },
		secondary: { label: 'Read the docs', href: '/guides/' },
		tertiary: { label: 'View on GitHub', href: 'https://github.com/gamesgamesgamesgamesgames/happyview' },
	},
}

export const sample = {
	title: 'Lexicons in, XRPC out',
	intro:
		'This is the whole project `hpy happyview create` gives you. The lexicon’s path is its route, and the script beside it answers it.',
	files: [
		{ path: 'happyview.toml', lang: 'toml', code: toml },
		{ path: 'src/lexicons/com/example/hello.json', lang: 'json', code: lexicon },
		{ path: 'src/lexicons/com/example/hello.lua', lang: 'lua', code: script },
	],
	run: 'hpy happyview dev',
	request: "curl 'http://127.0.0.1:3000/xrpc/com.example.hello?name=Ada'",
	response: '{"message":"Hello, Ada!"}',
	after: 'Change the script, save it and call it again: the new version answers, with no restart.',
	test: {
		intro: 'Tests fire the same request in process, with no network:',
		path: 'src/lexicons/com/example/hello.test.lua',
		lang: 'lua',
		code: test,
		run: 'hpy happyview test',
		output: ' PASS  src/lexicons/com/example/hello.test.lua (1 tests) 124ms',
	},
}

// Six tiles. `alternates` holds two more that are just as checkable, if the
// layout wants a swap.
export const capabilities = [
	{
		title: 'Your lexicons are your API',
		blurb:
			'Put a query or procedure lexicon in your project and its NSID is a live XRPC route. There is no router to write or keep in sync.',
	},
	{
		title: 'Scripts in Lua, JavaScript or TypeScript',
		blurb:
			'Each language is an interpreter plugin from the registry. Install the ones you use; a script runs on the interpreter for its language.',
	},
	{
		title: 'React to the network',
		blurb:
			'Run a script when a record in your collections is created, updated or deleted, or when a labeler you subscribe to labels something. Backfill pulls in what was there before you.',
	},
	{
		title: 'Jobs for the long work',
		blurb:
			'Hand slow, bulk work to a background job. Jobs report progress, can be paused and cancelled, and aren’t bound by a request’s time limit.',
	},
	{
		title: 'Plugins that say what they do',
		blurb:
			'Plugins are WebAssembly modules that declare their capabilities. HappyView checks the list against what the code imports and shows it to you before you install.',
	},
	{
		title: 'Test it like any other code',
		blurb:
			'`hpy happyview test` fires real queries, procedures, record events and jobs through HappyView in process. Nothing reaches the live network.',
	},
]

export const alternates = [
	{
		title: 'Access you can hand out',
		blurb:
			'Give each teammate exactly what they need: 52 permissions, four templates from Viewer to Full Access, and atproto OAuth sign-in for the dashboard.',
	},
	{
		title: 'SQLite or Postgres',
		blurb:
			'Start on SQLite with one file and nothing to operate. Move to Postgres when you need replicas; migrations run on startup on either.',
	},
]

export const hosting = {
	title: 'Run it yourself, or let HappyProto run it',
	intro: 'It’s the same HappyView and the same project either way.',
	selfHost: {
		title: 'Self-host',
		points: [
			'MIT licensed. The source, issues and releases are on GitHub.',
			'A single binary for macOS, Linux and Windows, or a Docker image.',
			'SQLite or Postgres.',
			'You run the server, TLS, upgrades and backups, and install the interpreters you need.',
		],
		cta: { label: 'Deploy HappyView', href: '/getting-started/deployment/' },
	},
	managed: {
		title: 'HappyProto',
		points: [
			'Your instance at your-app.hpy.at, with Lua already installed.',
			'Deploy a project from your terminal with `hpy happyview deploy` (hpy 1.0 beta).',
			'Three plan sizes, from 512 MB of memory and 5 GB of storage.',
			'In beta now, by invite.',
		],
		cta: { label: 'Join the HappyProto beta', href: 'https://happyproto.at' },
	},
}
