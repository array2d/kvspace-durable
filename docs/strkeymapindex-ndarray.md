# Map members and sparse coordinates

A map's main key stores a class-0 XValue with an empty body. Its langtype is the full `key-type·value-type` expression. For example, `/m` can hold `[int64,int64]·float32`. There is no separate index matrix or stored logical shape.

Each member is an independent physical key under `/m·`. The member `/m·[1,2]` is a scalar `float32` XValue; its coordinates are in the key, not in the parent's body. Nested maps use the same rule recursively, such as `/m·[1,2]·field`. Directory children use `/` as the separator. Listing derives direct members from physical prefixes, sorts coordinate names numerically, and suppresses duplicate directory names.

The parent value must exist before writing a map member. A missing coordinate has no key and reads as `None`. Deleting or copying a tree acts on its main key and member prefix together.
