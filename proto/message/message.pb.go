// Code generated by protoc-gen-go. DO NOT EDIT.
// versions:
// 	protoc-gen-go v1.36.0
// 	protoc        v5.29.1
// source: proto/message/message.proto

package pb

import (
	protoreflect "google.golang.org/protobuf/reflect/protoreflect"
	protoimpl "google.golang.org/protobuf/runtime/protoimpl"
	reflect "reflect"
	sync "sync"
)

const (
	// Verify that this generated code is sufficiently up-to-date.
	_ = protoimpl.EnforceVersion(20 - protoimpl.MinVersion)
	// Verify that runtime/protoimpl is sufficiently up-to-date.
	_ = protoimpl.EnforceVersion(protoimpl.MaxVersion - 20)
)

type MessageType int32

const (
	MessageType_UNKNOWN              MessageType = 0
	MessageType_ERROR                MessageType = 1
	MessageType_CHAT                 MessageType = 2
	MessageType_STATUS               MessageType = 3
	MessageType_CHECK_STATUS         MessageType = 4
	MessageType_EXPIRED              MessageType = 5
	MessageType_CURRENT              MessageType = 6
	MessageType_MOVIES               MessageType = 7
	MessageType_VIEWER_COUNT         MessageType = 8
	MessageType_SYNC                 MessageType = 9
	MessageType_MY_STATUS            MessageType = 10
	MessageType_WEBRTC_OFFER         MessageType = 11
	MessageType_WEBRTC_ANSWER        MessageType = 12
	MessageType_WEBRTC_ICE_CANDIDATE MessageType = 13
	MessageType_WEBRTC_JOIN          MessageType = 14
	MessageType_WEBRTC_LEAVE         MessageType = 15
)

// Enum value maps for MessageType.
var (
	MessageType_name = map[int32]string{
		0:  "UNKNOWN",
		1:  "ERROR",
		2:  "CHAT",
		3:  "STATUS",
		4:  "CHECK_STATUS",
		5:  "EXPIRED",
		6:  "CURRENT",
		7:  "MOVIES",
		8:  "VIEWER_COUNT",
		9:  "SYNC",
		10: "MY_STATUS",
		11: "WEBRTC_OFFER",
		12: "WEBRTC_ANSWER",
		13: "WEBRTC_ICE_CANDIDATE",
		14: "WEBRTC_JOIN",
		15: "WEBRTC_LEAVE",
	}
	MessageType_value = map[string]int32{
		"UNKNOWN":              0,
		"ERROR":                1,
		"CHAT":                 2,
		"STATUS":               3,
		"CHECK_STATUS":         4,
		"EXPIRED":              5,
		"CURRENT":              6,
		"MOVIES":               7,
		"VIEWER_COUNT":         8,
		"SYNC":                 9,
		"MY_STATUS":            10,
		"WEBRTC_OFFER":         11,
		"WEBRTC_ANSWER":        12,
		"WEBRTC_ICE_CANDIDATE": 13,
		"WEBRTC_JOIN":          14,
		"WEBRTC_LEAVE":         15,
	}
)

func (x MessageType) Enum() *MessageType {
	p := new(MessageType)
	*p = x
	return p
}

func (x MessageType) String() string {
	return protoimpl.X.EnumStringOf(x.Descriptor(), protoreflect.EnumNumber(x))
}

func (MessageType) Descriptor() protoreflect.EnumDescriptor {
	return file_proto_message_message_proto_enumTypes[0].Descriptor()
}

func (MessageType) Type() protoreflect.EnumType {
	return &file_proto_message_message_proto_enumTypes[0]
}

func (x MessageType) Number() protoreflect.EnumNumber {
	return protoreflect.EnumNumber(x)
}

// Deprecated: Use MessageType.Descriptor instead.
func (MessageType) EnumDescriptor() ([]byte, []int) {
	return file_proto_message_message_proto_rawDescGZIP(), []int{0}
}

type Sender struct {
	state         protoimpl.MessageState `protogen:"open.v1"`
	UserId        string                 `protobuf:"bytes,1,opt,name=user_id,json=userId,proto3" json:"user_id,omitempty"`
	Username      string                 `protobuf:"bytes,2,opt,name=username,proto3" json:"username,omitempty"`
	unknownFields protoimpl.UnknownFields
	sizeCache     protoimpl.SizeCache
}

func (x *Sender) Reset() {
	*x = Sender{}
	mi := &file_proto_message_message_proto_msgTypes[0]
	ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
	ms.StoreMessageInfo(mi)
}

func (x *Sender) String() string {
	return protoimpl.X.MessageStringOf(x)
}

func (*Sender) ProtoMessage() {}

func (x *Sender) ProtoReflect() protoreflect.Message {
	mi := &file_proto_message_message_proto_msgTypes[0]
	if x != nil {
		ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
		if ms.LoadMessageInfo() == nil {
			ms.StoreMessageInfo(mi)
		}
		return ms
	}
	return mi.MessageOf(x)
}

// Deprecated: Use Sender.ProtoReflect.Descriptor instead.
func (*Sender) Descriptor() ([]byte, []int) {
	return file_proto_message_message_proto_rawDescGZIP(), []int{0}
}

func (x *Sender) GetUserId() string {
	if x != nil {
		return x.UserId
	}
	return ""
}

func (x *Sender) GetUsername() string {
	if x != nil {
		return x.Username
	}
	return ""
}

type Status struct {
	state         protoimpl.MessageState `protogen:"open.v1"`
	IsPlaying     bool                   `protobuf:"varint,1,opt,name=is_playing,json=isPlaying,proto3" json:"is_playing,omitempty"`
	CurrentTime   float64                `protobuf:"fixed64,2,opt,name=current_time,json=currentTime,proto3" json:"current_time,omitempty"`
	PlaybackRate  float64                `protobuf:"fixed64,3,opt,name=playback_rate,json=playbackRate,proto3" json:"playback_rate,omitempty"`
	unknownFields protoimpl.UnknownFields
	sizeCache     protoimpl.SizeCache
}

func (x *Status) Reset() {
	*x = Status{}
	mi := &file_proto_message_message_proto_msgTypes[1]
	ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
	ms.StoreMessageInfo(mi)
}

func (x *Status) String() string {
	return protoimpl.X.MessageStringOf(x)
}

func (*Status) ProtoMessage() {}

func (x *Status) ProtoReflect() protoreflect.Message {
	mi := &file_proto_message_message_proto_msgTypes[1]
	if x != nil {
		ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
		if ms.LoadMessageInfo() == nil {
			ms.StoreMessageInfo(mi)
		}
		return ms
	}
	return mi.MessageOf(x)
}

// Deprecated: Use Status.ProtoReflect.Descriptor instead.
func (*Status) Descriptor() ([]byte, []int) {
	return file_proto_message_message_proto_rawDescGZIP(), []int{1}
}

func (x *Status) GetIsPlaying() bool {
	if x != nil {
		return x.IsPlaying
	}
	return false
}

func (x *Status) GetCurrentTime() float64 {
	if x != nil {
		return x.CurrentTime
	}
	return 0
}

func (x *Status) GetPlaybackRate() float64 {
	if x != nil {
		return x.PlaybackRate
	}
	return 0
}

type WebRTCData struct {
	state         protoimpl.MessageState `protogen:"open.v1"`
	Data          string                 `protobuf:"bytes,1,opt,name=data,proto3" json:"data,omitempty"`
	To            string                 `protobuf:"bytes,2,opt,name=to,proto3" json:"to,omitempty"`
	From          string                 `protobuf:"bytes,3,opt,name=from,proto3" json:"from,omitempty"`
	unknownFields protoimpl.UnknownFields
	sizeCache     protoimpl.SizeCache
}

func (x *WebRTCData) Reset() {
	*x = WebRTCData{}
	mi := &file_proto_message_message_proto_msgTypes[2]
	ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
	ms.StoreMessageInfo(mi)
}

func (x *WebRTCData) String() string {
	return protoimpl.X.MessageStringOf(x)
}

func (*WebRTCData) ProtoMessage() {}

func (x *WebRTCData) ProtoReflect() protoreflect.Message {
	mi := &file_proto_message_message_proto_msgTypes[2]
	if x != nil {
		ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
		if ms.LoadMessageInfo() == nil {
			ms.StoreMessageInfo(mi)
		}
		return ms
	}
	return mi.MessageOf(x)
}

// Deprecated: Use WebRTCData.ProtoReflect.Descriptor instead.
func (*WebRTCData) Descriptor() ([]byte, []int) {
	return file_proto_message_message_proto_rawDescGZIP(), []int{2}
}

func (x *WebRTCData) GetData() string {
	if x != nil {
		return x.Data
	}
	return ""
}

func (x *WebRTCData) GetTo() string {
	if x != nil {
		return x.To
	}
	return ""
}

func (x *WebRTCData) GetFrom() string {
	if x != nil {
		return x.From
	}
	return ""
}

type Message struct {
	state     protoimpl.MessageState `protogen:"open.v1"`
	Type      MessageType            `protobuf:"varint,1,opt,name=type,proto3,enum=proto.MessageType" json:"type,omitempty"`
	Timestamp int64                  `protobuf:"fixed64,2,opt,name=timestamp,proto3" json:"timestamp,omitempty"`
	Sender    *Sender                `protobuf:"bytes,3,opt,name=sender,proto3,oneof" json:"sender,omitempty"`
	// Types that are valid to be assigned to Payload:
	//
	//	*Message_ErrorMessage
	//	*Message_ChatContent
	//	*Message_PlaybackStatus
	//	*Message_ExpirationId
	//	*Message_ViewerCount
	//	*Message_WebrtcData
	Payload       isMessage_Payload `protobuf_oneof:"payload"`
	unknownFields protoimpl.UnknownFields
	sizeCache     protoimpl.SizeCache
}

func (x *Message) Reset() {
	*x = Message{}
	mi := &file_proto_message_message_proto_msgTypes[3]
	ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
	ms.StoreMessageInfo(mi)
}

func (x *Message) String() string {
	return protoimpl.X.MessageStringOf(x)
}

func (*Message) ProtoMessage() {}

func (x *Message) ProtoReflect() protoreflect.Message {
	mi := &file_proto_message_message_proto_msgTypes[3]
	if x != nil {
		ms := protoimpl.X.MessageStateOf(protoimpl.Pointer(x))
		if ms.LoadMessageInfo() == nil {
			ms.StoreMessageInfo(mi)
		}
		return ms
	}
	return mi.MessageOf(x)
}

// Deprecated: Use Message.ProtoReflect.Descriptor instead.
func (*Message) Descriptor() ([]byte, []int) {
	return file_proto_message_message_proto_rawDescGZIP(), []int{3}
}

func (x *Message) GetType() MessageType {
	if x != nil {
		return x.Type
	}
	return MessageType_UNKNOWN
}

func (x *Message) GetTimestamp() int64 {
	if x != nil {
		return x.Timestamp
	}
	return 0
}

func (x *Message) GetSender() *Sender {
	if x != nil {
		return x.Sender
	}
	return nil
}

func (x *Message) GetPayload() isMessage_Payload {
	if x != nil {
		return x.Payload
	}
	return nil
}

func (x *Message) GetErrorMessage() string {
	if x != nil {
		if x, ok := x.Payload.(*Message_ErrorMessage); ok {
			return x.ErrorMessage
		}
	}
	return ""
}

func (x *Message) GetChatContent() string {
	if x != nil {
		if x, ok := x.Payload.(*Message_ChatContent); ok {
			return x.ChatContent
		}
	}
	return ""
}

func (x *Message) GetPlaybackStatus() *Status {
	if x != nil {
		if x, ok := x.Payload.(*Message_PlaybackStatus); ok {
			return x.PlaybackStatus
		}
	}
	return nil
}

func (x *Message) GetExpirationId() uint64 {
	if x != nil {
		if x, ok := x.Payload.(*Message_ExpirationId); ok {
			return x.ExpirationId
		}
	}
	return 0
}

func (x *Message) GetViewerCount() int64 {
	if x != nil {
		if x, ok := x.Payload.(*Message_ViewerCount); ok {
			return x.ViewerCount
		}
	}
	return 0
}

func (x *Message) GetWebrtcData() *WebRTCData {
	if x != nil {
		if x, ok := x.Payload.(*Message_WebrtcData); ok {
			return x.WebrtcData
		}
	}
	return nil
}

type isMessage_Payload interface {
	isMessage_Payload()
}

type Message_ErrorMessage struct {
	ErrorMessage string `protobuf:"bytes,4,opt,name=error_message,json=errorMessage,proto3,oneof"`
}

type Message_ChatContent struct {
	ChatContent string `protobuf:"bytes,5,opt,name=chat_content,json=chatContent,proto3,oneof"`
}

type Message_PlaybackStatus struct {
	PlaybackStatus *Status `protobuf:"bytes,6,opt,name=playback_status,json=playbackStatus,proto3,oneof"`
}

type Message_ExpirationId struct {
	ExpirationId uint64 `protobuf:"fixed64,7,opt,name=expiration_id,json=expirationId,proto3,oneof"`
}

type Message_ViewerCount struct {
	ViewerCount int64 `protobuf:"varint,8,opt,name=viewer_count,json=viewerCount,proto3,oneof"`
}

type Message_WebrtcData struct {
	WebrtcData *WebRTCData `protobuf:"bytes,9,opt,name=webrtc_data,json=webrtcData,proto3,oneof"`
}

func (*Message_ErrorMessage) isMessage_Payload() {}

func (*Message_ChatContent) isMessage_Payload() {}

func (*Message_PlaybackStatus) isMessage_Payload() {}

func (*Message_ExpirationId) isMessage_Payload() {}

func (*Message_ViewerCount) isMessage_Payload() {}

func (*Message_WebrtcData) isMessage_Payload() {}

var File_proto_message_message_proto protoreflect.FileDescriptor

var file_proto_message_message_proto_rawDesc = []byte{
	0x0a, 0x1b, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x2f, 0x6d, 0x65, 0x73, 0x73, 0x61, 0x67, 0x65, 0x2f,
	0x6d, 0x65, 0x73, 0x73, 0x61, 0x67, 0x65, 0x2e, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x12, 0x05, 0x70,
	0x72, 0x6f, 0x74, 0x6f, 0x22, 0x3d, 0x0a, 0x06, 0x53, 0x65, 0x6e, 0x64, 0x65, 0x72, 0x12, 0x17,
	0x0a, 0x07, 0x75, 0x73, 0x65, 0x72, 0x5f, 0x69, 0x64, 0x18, 0x01, 0x20, 0x01, 0x28, 0x09, 0x52,
	0x06, 0x75, 0x73, 0x65, 0x72, 0x49, 0x64, 0x12, 0x1a, 0x0a, 0x08, 0x75, 0x73, 0x65, 0x72, 0x6e,
	0x61, 0x6d, 0x65, 0x18, 0x02, 0x20, 0x01, 0x28, 0x09, 0x52, 0x08, 0x75, 0x73, 0x65, 0x72, 0x6e,
	0x61, 0x6d, 0x65, 0x22, 0x6f, 0x0a, 0x06, 0x53, 0x74, 0x61, 0x74, 0x75, 0x73, 0x12, 0x1d, 0x0a,
	0x0a, 0x69, 0x73, 0x5f, 0x70, 0x6c, 0x61, 0x79, 0x69, 0x6e, 0x67, 0x18, 0x01, 0x20, 0x01, 0x28,
	0x08, 0x52, 0x09, 0x69, 0x73, 0x50, 0x6c, 0x61, 0x79, 0x69, 0x6e, 0x67, 0x12, 0x21, 0x0a, 0x0c,
	0x63, 0x75, 0x72, 0x72, 0x65, 0x6e, 0x74, 0x5f, 0x74, 0x69, 0x6d, 0x65, 0x18, 0x02, 0x20, 0x01,
	0x28, 0x01, 0x52, 0x0b, 0x63, 0x75, 0x72, 0x72, 0x65, 0x6e, 0x74, 0x54, 0x69, 0x6d, 0x65, 0x12,
	0x23, 0x0a, 0x0d, 0x70, 0x6c, 0x61, 0x79, 0x62, 0x61, 0x63, 0x6b, 0x5f, 0x72, 0x61, 0x74, 0x65,
	0x18, 0x03, 0x20, 0x01, 0x28, 0x01, 0x52, 0x0c, 0x70, 0x6c, 0x61, 0x79, 0x62, 0x61, 0x63, 0x6b,
	0x52, 0x61, 0x74, 0x65, 0x22, 0x44, 0x0a, 0x0a, 0x57, 0x65, 0x62, 0x52, 0x54, 0x43, 0x44, 0x61,
	0x74, 0x61, 0x12, 0x12, 0x0a, 0x04, 0x64, 0x61, 0x74, 0x61, 0x18, 0x01, 0x20, 0x01, 0x28, 0x09,
	0x52, 0x04, 0x64, 0x61, 0x74, 0x61, 0x12, 0x0e, 0x0a, 0x02, 0x74, 0x6f, 0x18, 0x02, 0x20, 0x01,
	0x28, 0x09, 0x52, 0x02, 0x74, 0x6f, 0x12, 0x12, 0x0a, 0x04, 0x66, 0x72, 0x6f, 0x6d, 0x18, 0x03,
	0x20, 0x01, 0x28, 0x09, 0x52, 0x04, 0x66, 0x72, 0x6f, 0x6d, 0x22, 0x99, 0x03, 0x0a, 0x07, 0x4d,
	0x65, 0x73, 0x73, 0x61, 0x67, 0x65, 0x12, 0x26, 0x0a, 0x04, 0x74, 0x79, 0x70, 0x65, 0x18, 0x01,
	0x20, 0x01, 0x28, 0x0e, 0x32, 0x12, 0x2e, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x2e, 0x4d, 0x65, 0x73,
	0x73, 0x61, 0x67, 0x65, 0x54, 0x79, 0x70, 0x65, 0x52, 0x04, 0x74, 0x79, 0x70, 0x65, 0x12, 0x1c,
	0x0a, 0x09, 0x74, 0x69, 0x6d, 0x65, 0x73, 0x74, 0x61, 0x6d, 0x70, 0x18, 0x02, 0x20, 0x01, 0x28,
	0x10, 0x52, 0x09, 0x74, 0x69, 0x6d, 0x65, 0x73, 0x74, 0x61, 0x6d, 0x70, 0x12, 0x2a, 0x0a, 0x06,
	0x73, 0x65, 0x6e, 0x64, 0x65, 0x72, 0x18, 0x03, 0x20, 0x01, 0x28, 0x0b, 0x32, 0x0d, 0x2e, 0x70,
	0x72, 0x6f, 0x74, 0x6f, 0x2e, 0x53, 0x65, 0x6e, 0x64, 0x65, 0x72, 0x48, 0x01, 0x52, 0x06, 0x73,
	0x65, 0x6e, 0x64, 0x65, 0x72, 0x88, 0x01, 0x01, 0x12, 0x25, 0x0a, 0x0d, 0x65, 0x72, 0x72, 0x6f,
	0x72, 0x5f, 0x6d, 0x65, 0x73, 0x73, 0x61, 0x67, 0x65, 0x18, 0x04, 0x20, 0x01, 0x28, 0x09, 0x48,
	0x00, 0x52, 0x0c, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x4d, 0x65, 0x73, 0x73, 0x61, 0x67, 0x65, 0x12,
	0x23, 0x0a, 0x0c, 0x63, 0x68, 0x61, 0x74, 0x5f, 0x63, 0x6f, 0x6e, 0x74, 0x65, 0x6e, 0x74, 0x18,
	0x05, 0x20, 0x01, 0x28, 0x09, 0x48, 0x00, 0x52, 0x0b, 0x63, 0x68, 0x61, 0x74, 0x43, 0x6f, 0x6e,
	0x74, 0x65, 0x6e, 0x74, 0x12, 0x38, 0x0a, 0x0f, 0x70, 0x6c, 0x61, 0x79, 0x62, 0x61, 0x63, 0x6b,
	0x5f, 0x73, 0x74, 0x61, 0x74, 0x75, 0x73, 0x18, 0x06, 0x20, 0x01, 0x28, 0x0b, 0x32, 0x0d, 0x2e,
	0x70, 0x72, 0x6f, 0x74, 0x6f, 0x2e, 0x53, 0x74, 0x61, 0x74, 0x75, 0x73, 0x48, 0x00, 0x52, 0x0e,
	0x70, 0x6c, 0x61, 0x79, 0x62, 0x61, 0x63, 0x6b, 0x53, 0x74, 0x61, 0x74, 0x75, 0x73, 0x12, 0x25,
	0x0a, 0x0d, 0x65, 0x78, 0x70, 0x69, 0x72, 0x61, 0x74, 0x69, 0x6f, 0x6e, 0x5f, 0x69, 0x64, 0x18,
	0x07, 0x20, 0x01, 0x28, 0x06, 0x48, 0x00, 0x52, 0x0c, 0x65, 0x78, 0x70, 0x69, 0x72, 0x61, 0x74,
	0x69, 0x6f, 0x6e, 0x49, 0x64, 0x12, 0x23, 0x0a, 0x0c, 0x76, 0x69, 0x65, 0x77, 0x65, 0x72, 0x5f,
	0x63, 0x6f, 0x75, 0x6e, 0x74, 0x18, 0x08, 0x20, 0x01, 0x28, 0x03, 0x48, 0x00, 0x52, 0x0b, 0x76,
	0x69, 0x65, 0x77, 0x65, 0x72, 0x43, 0x6f, 0x75, 0x6e, 0x74, 0x12, 0x34, 0x0a, 0x0b, 0x77, 0x65,
	0x62, 0x72, 0x74, 0x63, 0x5f, 0x64, 0x61, 0x74, 0x61, 0x18, 0x09, 0x20, 0x01, 0x28, 0x0b, 0x32,
	0x11, 0x2e, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x2e, 0x57, 0x65, 0x62, 0x52, 0x54, 0x43, 0x44, 0x61,
	0x74, 0x61, 0x48, 0x00, 0x52, 0x0a, 0x77, 0x65, 0x62, 0x72, 0x74, 0x63, 0x44, 0x61, 0x74, 0x61,
	0x42, 0x09, 0x0a, 0x07, 0x70, 0x61, 0x79, 0x6c, 0x6f, 0x61, 0x64, 0x42, 0x09, 0x0a, 0x07, 0x5f,
	0x73, 0x65, 0x6e, 0x64, 0x65, 0x72, 0x2a, 0x80, 0x02, 0x0a, 0x0b, 0x4d, 0x65, 0x73, 0x73, 0x61,
	0x67, 0x65, 0x54, 0x79, 0x70, 0x65, 0x12, 0x0b, 0x0a, 0x07, 0x55, 0x4e, 0x4b, 0x4e, 0x4f, 0x57,
	0x4e, 0x10, 0x00, 0x12, 0x09, 0x0a, 0x05, 0x45, 0x52, 0x52, 0x4f, 0x52, 0x10, 0x01, 0x12, 0x08,
	0x0a, 0x04, 0x43, 0x48, 0x41, 0x54, 0x10, 0x02, 0x12, 0x0a, 0x0a, 0x06, 0x53, 0x54, 0x41, 0x54,
	0x55, 0x53, 0x10, 0x03, 0x12, 0x10, 0x0a, 0x0c, 0x43, 0x48, 0x45, 0x43, 0x4b, 0x5f, 0x53, 0x54,
	0x41, 0x54, 0x55, 0x53, 0x10, 0x04, 0x12, 0x0b, 0x0a, 0x07, 0x45, 0x58, 0x50, 0x49, 0x52, 0x45,
	0x44, 0x10, 0x05, 0x12, 0x0b, 0x0a, 0x07, 0x43, 0x55, 0x52, 0x52, 0x45, 0x4e, 0x54, 0x10, 0x06,
	0x12, 0x0a, 0x0a, 0x06, 0x4d, 0x4f, 0x56, 0x49, 0x45, 0x53, 0x10, 0x07, 0x12, 0x10, 0x0a, 0x0c,
	0x56, 0x49, 0x45, 0x57, 0x45, 0x52, 0x5f, 0x43, 0x4f, 0x55, 0x4e, 0x54, 0x10, 0x08, 0x12, 0x08,
	0x0a, 0x04, 0x53, 0x59, 0x4e, 0x43, 0x10, 0x09, 0x12, 0x0d, 0x0a, 0x09, 0x4d, 0x59, 0x5f, 0x53,
	0x54, 0x41, 0x54, 0x55, 0x53, 0x10, 0x0a, 0x12, 0x10, 0x0a, 0x0c, 0x57, 0x45, 0x42, 0x52, 0x54,
	0x43, 0x5f, 0x4f, 0x46, 0x46, 0x45, 0x52, 0x10, 0x0b, 0x12, 0x11, 0x0a, 0x0d, 0x57, 0x45, 0x42,
	0x52, 0x54, 0x43, 0x5f, 0x41, 0x4e, 0x53, 0x57, 0x45, 0x52, 0x10, 0x0c, 0x12, 0x18, 0x0a, 0x14,
	0x57, 0x45, 0x42, 0x52, 0x54, 0x43, 0x5f, 0x49, 0x43, 0x45, 0x5f, 0x43, 0x41, 0x4e, 0x44, 0x49,
	0x44, 0x41, 0x54, 0x45, 0x10, 0x0d, 0x12, 0x0f, 0x0a, 0x0b, 0x57, 0x45, 0x42, 0x52, 0x54, 0x43,
	0x5f, 0x4a, 0x4f, 0x49, 0x4e, 0x10, 0x0e, 0x12, 0x10, 0x0a, 0x0c, 0x57, 0x45, 0x42, 0x52, 0x54,
	0x43, 0x5f, 0x4c, 0x45, 0x41, 0x56, 0x45, 0x10, 0x0f, 0x42, 0x06, 0x5a, 0x04, 0x2e, 0x3b, 0x70,
	0x62, 0x62, 0x06, 0x70, 0x72, 0x6f, 0x74, 0x6f, 0x33,
}

var (
	file_proto_message_message_proto_rawDescOnce sync.Once
	file_proto_message_message_proto_rawDescData = file_proto_message_message_proto_rawDesc
)

func file_proto_message_message_proto_rawDescGZIP() []byte {
	file_proto_message_message_proto_rawDescOnce.Do(func() {
		file_proto_message_message_proto_rawDescData = protoimpl.X.CompressGZIP(file_proto_message_message_proto_rawDescData)
	})
	return file_proto_message_message_proto_rawDescData
}

var file_proto_message_message_proto_enumTypes = make([]protoimpl.EnumInfo, 1)
var file_proto_message_message_proto_msgTypes = make([]protoimpl.MessageInfo, 4)
var file_proto_message_message_proto_goTypes = []any{
	(MessageType)(0),   // 0: proto.MessageType
	(*Sender)(nil),     // 1: proto.Sender
	(*Status)(nil),     // 2: proto.Status
	(*WebRTCData)(nil), // 3: proto.WebRTCData
	(*Message)(nil),    // 4: proto.Message
}
var file_proto_message_message_proto_depIdxs = []int32{
	0, // 0: proto.Message.type:type_name -> proto.MessageType
	1, // 1: proto.Message.sender:type_name -> proto.Sender
	2, // 2: proto.Message.playback_status:type_name -> proto.Status
	3, // 3: proto.Message.webrtc_data:type_name -> proto.WebRTCData
	4, // [4:4] is the sub-list for method output_type
	4, // [4:4] is the sub-list for method input_type
	4, // [4:4] is the sub-list for extension type_name
	4, // [4:4] is the sub-list for extension extendee
	0, // [0:4] is the sub-list for field type_name
}

func init() { file_proto_message_message_proto_init() }
func file_proto_message_message_proto_init() {
	if File_proto_message_message_proto != nil {
		return
	}
	file_proto_message_message_proto_msgTypes[3].OneofWrappers = []any{
		(*Message_ErrorMessage)(nil),
		(*Message_ChatContent)(nil),
		(*Message_PlaybackStatus)(nil),
		(*Message_ExpirationId)(nil),
		(*Message_ViewerCount)(nil),
		(*Message_WebrtcData)(nil),
	}
	type x struct{}
	out := protoimpl.TypeBuilder{
		File: protoimpl.DescBuilder{
			GoPackagePath: reflect.TypeOf(x{}).PkgPath(),
			RawDescriptor: file_proto_message_message_proto_rawDesc,
			NumEnums:      1,
			NumMessages:   4,
			NumExtensions: 0,
			NumServices:   0,
		},
		GoTypes:           file_proto_message_message_proto_goTypes,
		DependencyIndexes: file_proto_message_message_proto_depIdxs,
		EnumInfos:         file_proto_message_message_proto_enumTypes,
		MessageInfos:      file_proto_message_message_proto_msgTypes,
	}.Build()
	File_proto_message_message_proto = out.File
	file_proto_message_message_proto_rawDesc = nil
	file_proto_message_message_proto_goTypes = nil
	file_proto_message_message_proto_depIdxs = nil
}
