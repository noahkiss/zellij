// Word lists for pane handles, vendored from the `petname` crate:
// https://github.com/allenap/rust-petname (Apache-2.0, see LICENSE beside this file).
//
// Taken from petname 2.0.2's `words/small` lists - plus `otter` and `puffin` from its `medium`
// nouns - then filtered by hand to words that are three to eight lowercase ascii letters, easy to
// spell from hearing them, and harmless in any pairing. Vendored rather than depended on: two
// const arrays do not warrant a dependency, and the fork needs the lists frozen anyway - see the
// append-only rule below.
//
// No word appears in both lists, which is what lets `sunny-otter` be split back into its parts.
//
// APPEND-ONLY. A word may be added, never removed or renamed. Handles are persisted into session
// snapshots, so dropping a word orphans every handle in an old snapshot that used it.

/// The first word of a pane handle.

pub const ADJECTIVES: [&str; 193] = [
    "able", "active", "alert", "alive", "amused", "ample", "artistic", "awake", "aware", "bold",
    "brave", "bright", "busy", "calm", "careful", "caring", "casual", "central", "certain",
    "cheerful", "chief", "civil", "classic", "clean", "clear", "clever", "comic", "concise",
    "cool", "cosmic", "crisp", "cunning", "curious", "cute", "daring", "dashing", "decent", "deep",
    "direct", "divine", "driven", "dynamic", "eager", "easy", "elegant", "endless", "epic",
    "exact", "exotic", "expert", "fair", "famous", "fancy", "fast", "fine", "firm", "fleet",
    "flying", "fond", "frank", "free", "fresh", "funky", "funny", "gentle", "glad", "glowing",
    "golden", "grand", "great", "handy", "happy", "hardy", "healthy", "helpful", "heroic",
    "honest", "hopeful", "humble", "ideal", "immense", "intense", "keen", "kind", "large",
    "lasting", "leading", "light", "living", "logical", "loyal", "lucky", "magical", "magnetic",
    "main", "major", "master", "mature", "merry", "mighty", "mint", "modern", "modest", "moving",
    "musical", "native", "natural", "neat", "neutral", "noble", "normal", "novel", "open",
    "organic", "patient", "peaceful", "perfect", "pleasant", "polished", "polite", "popular",
    "precious", "precise", "premium", "pretty", "prime", "prompt", "proper", "proud", "pure",
    "quick", "quiet", "rapid", "rare", "ready", "refined", "regular", "relaxed", "resolved",
    "rested", "rich", "robust", "romantic", "sacred", "safe", "secure", "sensible", "sharp",
    "shining", "simple", "sincere", "skilled", "smart", "smiling", "smooth", "social", "solid",
    "sound", "special", "splendid", "square", "stable", "steady", "sterling", "striking", "strong",
    "stunning", "subtle", "sunny", "super", "superb", "supreme", "sweet", "talented", "tender",
    "thorough", "tidy", "tough", "true", "trusty", "unique", "useful", "valid", "vast", "vital",
    "vocal", "warm", "welcome", "willing", "winning", "wise", "witty", "worthy",
];

/// The second word of a pane handle.

pub const NOUNS: [&str; 287] = [
    "ant", "ape", "bat", "bee", "boa", "bug", "cat", "cod", "cow", "cub", "doe", "dog", "eel",
    "elk", "emu", "fly", "fox", "gnu", "hen", "hog", "jay", "koi", "owl", "pig", "pug", "pup",
    "ram", "ray", "yak", "bass", "bear", "bird", "boar", "buck", "bull", "calf", "clam", "colt",
    "crab", "crow", "deer", "dodo", "dove", "duck", "fawn", "fish", "foal", "frog", "goat", "gull",
    "hare", "hawk", "ibex", "joey", "kite", "kiwi", "lamb", "lark", "lion", "loon", "lynx", "mako",
    "mink", "mole", "moth", "mule", "newt", "orca", "oryx", "pika", "pony", "puma", "seal", "stag",
    "swan", "teal", "toad", "tuna", "wasp", "wolf", "wren", "adder", "akita", "bison", "boxer",
    "bunny", "burro", "camel", "chimp", "civet", "cobra", "coral", "corgi", "crane", "dingo",
    "drake", "eagle", "egret", "filly", "finch", "gecko", "goose", "guppy", "heron", "hippo",
    "horse", "hound", "husky", "hyena", "koala", "krill", "lemur", "liger", "llama", "macaw",
    "moose", "mouse", "panda", "perch", "prawn", "quail", "raven", "rhino", "robin", "shark",
    "sheep", "shrew", "skink", "skunk", "sloth", "snail", "snake", "snipe", "squid", "stork",
    "swift", "tapir", "tetra", "tiger", "trout", "viper", "whale", "zebra", "alpaca", "baboon",
    "badger", "beagle", "beetle", "bobcat", "caiman", "cicada", "collie", "condor", "cougar",
    "coyote", "dragon", "falcon", "ferret", "gibbon", "gopher", "grouse", "hornet", "iguana",
    "impala", "jackal", "jaguar", "kitten", "kodiak", "lizard", "magpie", "mantis", "marlin",
    "marmot", "mayfly", "minnow", "monkey", "muskox", "ocelot", "oriole", "osprey", "oyster",
    "parrot", "pigeon", "piglet", "poodle", "possum", "python", "rabbit", "raptor", "salmon",
    "shrimp", "spider", "sponge", "thrush", "toucan", "turkey", "turtle", "urchin", "walrus",
    "weasel", "wombat", "anchovy", "bluejay", "buffalo", "bulldog", "caribou", "cheetah",
    "chicken", "cricket", "dolphin", "firefly", "gazelle", "giraffe", "gorilla", "grizzly",
    "hamster", "herring", "ladybug", "leopard", "lobster", "mallard", "mammoth", "manatee",
    "mastiff", "meerkat", "monarch", "narwhal", "octopus", "ostrich", "panther", "peacock",
    "pelican", "penguin", "phoenix", "piranha", "raccoon", "rooster", "seagull", "skylark",
    "snapper", "spaniel", "sparrow", "sunbeam", "sunfish", "tadpole", "terrier", "unicorn",
    "vulture", "wallaby", "wildcat", "aardvark", "anteater", "antelope", "barnacle", "bluebird",
    "bullfrog", "cardinal", "chipmunk", "crayfish", "dinosaur", "duckling", "elephant", "flamingo",
    "goldfish", "hedgehog", "honeybee", "kangaroo", "labrador", "mackerel", "mongoose", "pangolin",
    "parakeet", "pheasant", "platypus", "porpoise", "reindeer", "ringtail", "sailfish", "scorpion",
    "seahorse", "squirrel", "stallion", "starfish", "stingray", "tortoise", "otter", "puffin",
];
