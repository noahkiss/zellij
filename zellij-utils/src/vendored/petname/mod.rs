// Word lists for pane handles, vendored from the `petname` crate:
// https://github.com/allenap/rust-petname (Apache-2.0, see LICENSE beside this file).
//
// Taken from petname 2.0.2's `words/small` lists - plus `otter` and `puffin` from its `medium`
// nouns - then filtered by hand to words that are three to eight lowercase ascii letters, easy to
// spell from hearing them, and harmless in any pairing. Vendored rather than depended on: two
// const arrays do not warrant a dependency, and the fork needs the lists frozen anyway - see the
// append-only rule below.
//
// Expanded for fixed-width handles: every adjective and noun here has a same-length-sum-11 partner
// on the other list, so a generated handle (adjective + '-' + noun) is always exactly 12 characters.
// Sorted by length, then alphabetically, within each list - this is cosmetic, not load-bearing;
// `pane_handle.rs` builds its own index over the pairs and does not lean on this order.
//
// No word appears in both lists, which is what lets `sunny-otter` be split back into its parts.
//
// APPEND-ONLY. A word may be added, never removed or renamed. Handles are persisted into session
// snapshots, so dropping a word orphans every handle in an old snapshot that used it. Adding a word
// changes the fixed-width pair space pane_handle.rs builds from these lists, so pair the addition
// with a matching partner on the other list at the length that keeps the sum at 11 if it is meant
// to be reachable by generation - otherwise it is still a valid word for a chosen or restored handle.

/// The first word of a pane handle.

pub const ADJECTIVES: [&str; 504] = [
    "ace", "apt", "big", "coy", "dry", "fab", "fit", "fun", "hip", "hot", "icy", "new", "odd",
    "pro", "raw", "red", "shy", "sly", "tan", "wee", "wet", "wry", "zen", "able", "airy", "aqua",
    "avid", "blue", "bold", "busy", "calm", "chic", "cold", "cool", "cozy", "cute", "dark", "dear",
    "deep", "deft", "dewy", "easy", "epic", "even", "fair", "fast", "fine", "firm", "fond", "free",
    "full", "glad", "gold", "hazy", "holy", "huge", "keen", "kind", "long", "lush", "main", "mega",
    "mild", "mini", "mint", "neat", "nice", "open", "oval", "pale", "pink", "posh", "prim", "pure",
    "rare", "rich", "rosy", "safe", "sage", "slim", "snug", "soft", "solo", "spry", "tall", "teal",
    "tidy", "tiny", "trim", "true", "vast", "warm", "wavy", "wide", "wild", "wiry", "wise", "zany",
    "adept", "agile", "aglow", "alert", "alive", "amber", "ample", "artsy", "awake", "aware",
    "azure", "balmy", "beefy", "bonny", "brave", "brisk", "burly", "chewy", "chill", "civil",
    "clean", "clear", "comfy", "crisp", "curly", "cushy", "dandy", "downy", "dusky", "eager",
    "ebony", "exact", "famed", "fancy", "frank", "fresh", "funky", "funny", "furry", "giant",
    "gooey", "goofy", "grand", "great", "gutsy", "handy", "happy", "hardy", "hefty", "homey",
    "ideal", "ivory", "jazzy", "jolly", "juicy", "jumbo", "khaki", "large", "leafy", "light",
    "lofty", "loyal", "lucky", "lunar", "major", "merry", "milky", "minty", "misty", "noble",
    "nutty", "peppy", "perky", "plaid", "plush", "prime", "primo", "proud", "quick", "quiet",
    "rainy", "rapid", "ready", "regal", "retro", "ritzy", "rocky", "roomy", "round", "sandy",
    "sassy", "sharp", "shiny", "silky", "silly", "sleek", "smart", "snowy", "solar", "solid",
    "spicy", "still", "stoic", "suave", "sunny", "super", "sweet", "swift", "tasty", "tidal",
    "tight", "tough", "turbo", "ultra", "valid", "vital", "vivid", "vocal", "wacky", "windy",
    "witty", "woody", "woven", "young", "yummy", "zesty", "zippy", "active", "alpine", "amazed",
    "amused", "arctic", "artful", "astute", "auburn", "brainy", "brawny", "breezy", "bright",
    "bubbly", "candid", "caring", "carved", "casual", "chatty", "cheery", "chirpy", "chummy",
    "classy", "clever", "cloudy", "cobalt", "copper", "cosmic", "curved", "dapper", "daring",
    "decent", "deluxe", "direct", "divine", "dreamy", "driven", "earthy", "elated", "exotic",
    "expert", "famous", "fluent", "fluffy", "flying", "folksy", "fruity", "genial", "gentle",
    "gifted", "gilded", "glossy", "golden", "grassy", "hearty", "heroic", "honest", "humble",
    "iconic", "indigo", "jovial", "joyful", "joyous", "kindly", "lively", "living", "lovely",
    "mature", "mellow", "mighty", "modern", "modest", "moving", "native", "nimble", "normal",
    "padded", "pastel", "peachy", "pearly", "petite", "placid", "plucky", "poetic", "poised",
    "polite", "potted", "pretty", "prized", "proper", "quirky", "rested", "robust", "rooted",
    "roving", "rugged", "rustic", "sacred", "scenic", "secure", "serene", "shaggy", "shrewd",
    "silent", "silken", "silver", "simple", "smooth", "snappy", "snazzy", "social", "spiffy",
    "spunky", "stable", "starry", "steady", "strong", "sturdy", "subtle", "sugary", "sunlit",
    "superb", "supple", "svelte", "tender", "toasty", "trendy", "trusty", "unique", "upbeat",
    "useful", "wintry", "wooded", "wooden", "woodsy", "woolen", "worthy", "affable", "amazing",
    "amiable", "amusing", "awesome", "bashful", "beaming", "blazing", "blessed", "bronzed",
    "careful", "central", "certain", "chipper", "classic", "coastal", "concise", "crimson",
    "cunning", "curious", "dancing", "dashing", "devoted", "durable", "dutiful", "dynamic",
    "earnest", "elegant", "emerald", "endless", "excited", "fervent", "festive", "gallant",
    "genuine", "gleeful", "gliding", "glowing", "healthy", "helpful", "hopeful", "immense",
    "intense", "lasting", "leading", "leaping", "logical", "magical", "mindful", "moonlit",
    "musical", "natural", "neutral", "organic", "perfect", "playful", "pleased", "popular",
    "precise", "premium", "purring", "radiant", "refined", "regular", "relaxed", "restful",
    "roaring", "scarlet", "shining", "sincere", "singing", "skilled", "slender", "smiling",
    "soaring", "soulful", "sparkly", "special", "starlit", "stellar", "stylish", "supreme",
    "tactful", "trusted", "untamed", "upright", "valiant", "verdant", "vibrant", "welcome",
    "willing", "winning", "zealous", "adorable", "amicable", "animated", "artistic", "blissful",
    "charming", "cheerful", "colorful", "dazzling", "diligent", "fabulous", "faithful", "fanciful",
    "fearless", "fruitful", "generous", "gleaming", "glorious", "gorgeous", "graceful", "gracious",
    "grateful", "handsome", "inviting", "jubilant", "luminous", "magnetic", "peaceful", "pleasant",
    "polished", "precious", "pristine", "punctual", "romantic", "sensible", "skillful", "soothing",
    "spacious", "spirited", "splendid", "sterling", "striking", "stunning", "talented", "thankful",
    "thorough", "tranquil", "tropical", "truthful", "youthful",
];

/// The second word of a pane handle.

pub const NOUNS: [&str; 594] = [
    "alp", "ant", "ape", "bag", "bat", "bee", "boa", "box", "cap", "car", "cat", "cod", "cow",
    "cub", "cup", "doe", "dog", "eel", "elf", "elk", "emu", "ewe", "fan", "fly", "fox", "gem",
    "hat", "hen", "hog", "hut", "jar", "jay", "jug", "key", "kit", "koi", "map", "mug", "orb",
    "owl", "pen", "pig", "pot", "pug", "pup", "ram", "ray", "rug", "ski", "sow", "spa", "toy",
    "van", "wok", "yak", "barn", "bass", "bear", "bell", "bird", "boar", "boat", "book", "boot",
    "bowl", "buck", "bull", "cake", "calf", "cape", "carp", "cart", "cave", "clam", "coin", "colt",
    "comb", "cove", "crab", "crow", "dawn", "deer", "dell", "desk", "dish", "dock", "dodo", "dojo",
    "door", "dove", "drum", "duck", "dune", "dusk", "fawn", "fern", "fish", "flag", "foal", "fork",
    "frog", "gale", "gift", "glen", "glow", "goat", "gull", "hare", "harp", "hawk", "hill", "horn",
    "isle", "kelp", "kite", "kiwi", "lake", "lamb", "lamp", "lark", "lava", "leaf", "lion", "loon",
    "luna", "lynx", "mako", "mare", "mask", "mast", "mesa", "mink", "mist", "mole", "moon", "moss",
    "moth", "mule", "newt", "orca", "oven", "pail", "palm", "peak", "pier", "pine", "pond", "pony",
    "puma", "reed", "reef", "ring", "robe", "root", "rope", "sack", "saga", "sail", "sand", "seal",
    "silo", "sink", "sled", "sofa", "stag", "star", "swan", "taco", "tent", "tide", "toad", "tote",
    "tray", "tuba", "tuna", "twig", "vase", "vine", "wand", "wasp", "wave", "wolf", "wren", "yeti",
    "acorn", "adder", "apron", "atlas", "attic", "banjo", "berry", "birch", "bison", "bluff",
    "boxer", "brick", "brook", "brush", "bunny", "cabin", "camel", "candy", "canoe", "cedar",
    "chair", "chalk", "charm", "chick", "chimp", "cloak", "clock", "cloud", "cobra", "comet",
    "coral", "corgi", "couch", "crane", "crate", "creek", "crest", "crown", "delta", "dingo",
    "drake", "eagle", "ember", "fence", "filly", "finch", "flame", "flare", "flute", "frost",
    "gecko", "glade", "glass", "globe", "gnome", "goose", "grain", "grove", "guppy", "hedge",
    "heron", "hippo", "horse", "hound", "husky", "igloo", "inlet", "kayak", "koala", "krill",
    "lemur", "llama", "macaw", "mango", "maple", "marsh", "medal", "moose", "mouse", "otter",
    "panda", "patio", "pearl", "penny", "perch", "petal", "piano", "plate", "plaza", "porch",
    "pouch", "prawn", "puppy", "quail", "quill", "quilt", "raven", "rhino", "ridge", "robin",
    "scarf", "shark", "sheep", "sheet", "shelf", "shell", "shore", "shrew", "skunk", "slope",
    "sloth", "snail", "snake", "spark", "spool", "spoon", "squid", "stone", "stork", "storm",
    "straw", "table", "tango", "tiger", "torch", "towel", "tower", "trout", "trunk", "vault",
    "viper", "wagon", "waltz", "whale", "wheat", "zebra", "alpaca", "amulet", "anchor", "aurora",
    "autumn", "baboon", "badger", "bangle", "banner", "barrel", "basket", "beacon", "beagle",
    "beaver", "beetle", "bistro", "bobcat", "branch", "breeze", "bridge", "brooch", "bucket",
    "button", "cabana", "candle", "canyon", "carpet", "castle", "cellar", "cicada", "collie",
    "condor", "cookie", "cougar", "coyote", "cradle", "cymbal", "desert", "donkey", "dragon",
    "falcon", "ferret", "fiddle", "galaxy", "garden", "gazebo", "gelato", "gibbon", "goblet",
    "gopher", "gravel", "grouse", "guitar", "hammer", "harbor", "helmet", "hornet", "impala",
    "jackal", "jacket", "jaguar", "jungle", "kettle", "kitten", "ladder", "lagoon", "lizard",
    "magpie", "mantis", "marlin", "marmot", "meadow", "meteor", "minnow", "mirror", "mitten",
    "monkey", "muffin", "napkin", "needle", "noodle", "ocelot", "osprey", "oyster", "parrot",
    "pebble", "pigeon", "piglet", "pillar", "pillow", "planet", "pocket", "poodle", "possum",
    "puffin", "python", "rabbit", "raptor", "ribbon", "rudder", "safari", "salmon", "sandal",
    "saucer", "scroll", "shrimp", "sonata", "spider", "sponge", "spring", "statue", "strait",
    "summer", "summit", "sunset", "teacup", "teapot", "thread", "toucan", "tundra", "tunnel",
    "turkey", "turtle", "valley", "violin", "waffle", "wallet", "walrus", "weasel", "window",
    "winter", "wombat", "zipper", "anchovy", "balcony", "bathtub", "biscuit", "blanket", "blossom",
    "boulder", "buffalo", "bulldog", "burrito", "cabinet", "caravan", "caribou", "cascade",
    "catfish", "chalice", "cheetah", "chicken", "chimney", "compass", "cricket", "cupcake",
    "dolphin", "firefly", "gazelle", "giraffe", "glacier", "gondola", "gorilla", "gosling",
    "grizzly", "halibut", "hammock", "hamster", "harvest", "herring", "horizon", "ladybug",
    "lantern", "lobster", "mailbox", "mallard", "mammoth", "manatee", "mastiff", "meerkat",
    "monarch", "narwhal", "octopus", "orchard", "ostrich", "panther", "peacock", "pelican",
    "penguin", "pennant", "phoenix", "pitcher", "plateau", "platter", "popcorn", "prairie",
    "pretzel", "rainbow", "redwood", "rooster", "sardine", "seagull", "shutter", "skillet",
    "skylark", "slipper", "snapper", "sneaker", "spaniel", "sparrow", "strudel", "sunbeam",
    "sundial", "tadpole", "terrier", "thicket", "trumpet", "tumbler", "unicorn", "volcano",
    "vulture", "wallaby", "whistle", "wildcat", "aardvark", "anteater", "antelope", "backpack",
    "backyard", "barnacle", "birdcage", "bluebird", "bullfrog", "campfire", "cardinal", "chipmunk",
    "crayfish", "cupboard", "dinosaur", "duckling", "elephant", "flamingo", "foothill", "fountain",
    "goldfish", "hedgehog", "hillside", "honeybee", "kangaroo", "labrador", "mandolin", "mongoose",
    "mountain", "necklace", "parakeet", "platypus", "porpoise", "raindrop", "reindeer", "sailboat",
    "sandwich", "scorpion", "seahorse", "seashell", "seashore", "snowball", "sombrero", "stallion",
    "starfish", "stingray", "sunshine", "tortoise", "treasure", "umbrella", "windmill",
];
