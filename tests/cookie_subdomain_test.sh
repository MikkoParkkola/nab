#!/bin/bash
# Test cookie subdomain matching
# Verifies that cookies on parent domains (e.g., .yle.fi) are correctly
# sent to subdomains (e.g., areena.yle.fi)

set -e

echo "Testing cookie subdomain matching..."

# Test 1: Subdomain should match parent domain cookies
OUTPUT1=$(./target/release/nab fetch https://areena.yle.fi --cookies brave 2>&1)
if [[ "$OUTPUT1" == *"Loaded"*"cookies"* ]] && [[ "$OUTPUT1" != *"0 cookies"* ]]; then
    echo "✅ Test 1 passed: Subdomain (areena.yle.fi) found parent domain cookies"
else
    echo "❌ Test 1 failed: Subdomain did not find cookies"
    exit 1
fi

# Test 2: Parent domain should also match
OUTPUT2=$(./target/release/nab fetch https://yle.fi --cookies brave 2>&1)
if [[ "$OUTPUT2" == *"Loaded"*"cookies"* ]] && [[ "$OUTPUT2" != *"0 cookies"* ]]; then
    echo "✅ Test 2 passed: Parent domain (yle.fi) found cookies"
else
    echo "❌ Test 2 failed: Parent domain did not find cookies"
    exit 1
fi

echo ""
echo "All cookie subdomain tests passed!"
