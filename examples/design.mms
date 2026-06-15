// Mimi 草图模式示例：从意图到契约

rule "所有支付路径必须有补偿策略"
rule "并发步骤失败时必须取消其余任务"

module Booking:
    desc "酒店 + 机票 + 支付组合预订"

    type BookingStatus:
        Pending
        Confirmed
        Cancelled

    type Reservation:
        seat_id: string
        hotel_id: string
        payment_id: string

    func book_trip:
        desc "预订完整行程"
        requires: user_id > 0
        ensures: status == Confirmed
        ...

    func cancel_trip(res: Reservation):
        desc "取消已预订行程"
        desc "补偿策略：先取消酒店，再取消座位"
        ...
