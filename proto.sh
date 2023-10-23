#!/bin/bash
protoc --go_out=./proto/message ./proto/message/*.proto
protoc --go_out=./proto/provider --go-grpc_out=./proto/provider ./proto/provider/*.proto
