#!/bin/sh

if [ -z ${KEYPATH+x} ]
then
	echo "\$KEYPATH environment variable must be set!"
	exit 1
fi

docker run --rm -v $KEYPATH:/tmp/key.json -it $(docker build -q .) ./drift-liquidator --keypath /tmp/key.json $@
