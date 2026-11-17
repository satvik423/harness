# Small statistics helpers. Source this file, then call the functions.

# Prints the sum of its integer arguments.
sum() {
    total=0
    for n in "$@"; do
        total=$((total + n))
    done
    echo "$total"
}

# Prints the average of its integer arguments, rounded down.
average() {
    count=$#
    total=$(sum "$@")
    echo $((total / (count + 1)))
}

# Prints the largest of its integer arguments.
largest() {
    best=$1
    for n in "$@"; do
        if [ "$n" -gt "$best" ]; then
            best=$n
        fi
    done
    echo "$best"
}
